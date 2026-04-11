// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

use core::sync::atomic::{AtomicU32, Ordering};

use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{Ipv4Address, Stack};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_println::println;
use static_cell::ConstStaticCell;

use crate::firmware::shared;

const NTP_PORT: u16 = 123;
const NTP_LOCAL_PORT: u16 = 1125;
const NTP_SYNC_INTERVAL_SECS: u64 = 1800;
const NTP_RETRY_SECS: u64 = 60;
const NTP_TIMEOUT_SECS: u64 = 5;
const NTP_MIN_POLL_SECS: u64 = 60;
const NTP_POLL_RAMP_DURATION_SECS: u64 = 600;
const NTP_UNIX_OFFSET: u32 = 2_208_988_800;
const NTP_SERVERS_CONFIG: Option<&str> = option_env!("NTP_SERVERS");
const NTP_SERVER_CONFIG: Option<&str> = option_env!("NTP_SERVER");

// 8 metadata slots: allows queuing responses from multiple servers (plus any
// late/duplicate responses from a previous server) while recv_from processes
// them in order.  With only 1 slot a fast response from server N may be
// dropped if a delayed response from server N-1 is already occupying the slot,
// leaving us to wait ~1 s for the server to retransmit.
static NTP_RX_META: ConstStaticCell<[PacketMetadata; 8]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; 8]);
static NTP_TX_META: ConstStaticCell<[PacketMetadata; 4]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; 4]);
// Each NTP packet is 48 bytes; 8 × 64 bytes is sufficient for 8 buffered responses.
static NTP_RX_BUFFER: ConstStaticCell<[u8; 512]> = ConstStaticCell::new([0; 512]);
// TX buffer: 4 × 64 bytes covers up to 4 simultaneous outbound NTP queries.
static NTP_TX_BUFFER: ConstStaticCell<[u8; 256]> = ConstStaticCell::new([0; 256]);
static NTP_QUERY_NONCE: AtomicU32 = AtomicU32::new(1);

#[derive(Clone, Copy)]
struct NtpSample {
    unix: u32,
    unix_frac_us: u32,
    recv_ticks: u64,
    stratum: u8,
    latency_ms: u32,
    jitter_ms: u32,
    latency_us: u32,
    offset_us: Option<i32>,
}

fn ntp_config_peers_from(
    config_list: Option<&str>,
    fallback_single: Option<&str>,
) -> heapless::Vec<Ipv4Address, { shared::NTP_MAX_CONFIG_SERVERS }> {
    shared::ntp_config_peers_from(config_list, fallback_single)
        .into_iter()
        .map(|octets| Ipv4Address::new(octets[0], octets[1], octets[2], octets[3]))
        .collect()
}

fn ntp_peer_candidates(
    stack: &Stack<'static>,
) -> (
    heapless::Vec<Ipv4Address, { shared::NTP_MAX_CONFIG_SERVERS }>,
    Option<Ipv4Address>,
) {
    let config_peers = ntp_config_peers_from(NTP_SERVERS_CONFIG, NTP_SERVER_CONFIG);
    let dhcp_peer = stack.config_v4().and_then(|cfg| cfg.gateway);
    (config_peers, dhcp_peer)
}

fn should_replace_master(current: NtpSample, candidate: NtpSample) -> bool {
    shared::should_replace_master(
        shared::NtpSelectionSample {
            stratum: current.stratum,
            latency_ms: current.latency_ms,
            jitter_ms: current.jitter_ms,
        },
        shared::NtpSelectionSample {
            stratum: candidate.stratum,
            latency_ms: candidate.latency_ms,
            jitter_ms: candidate.jitter_ms,
        },
    )
}

fn ntp_poll_interval_secs_after_sync(age_since_sync_s: u64) -> u64 {
    if age_since_sync_s >= NTP_POLL_RAMP_DURATION_SECS {
        return NTP_SYNC_INTERVAL_SECS;
    }

    let span = NTP_SYNC_INTERVAL_SECS.saturating_sub(NTP_MIN_POLL_SECS);
    NTP_MIN_POLL_SECS.saturating_add(
        age_since_sync_s
            .saturating_mul(span)
            .saturating_add(NTP_POLL_RAMP_DURATION_SECS / 2)
            / NTP_POLL_RAMP_DURATION_SECS,
    )
}

// Maximum pending NTP queries in flight simultaneously (config + DHCP gateway).
const NTP_MAX_PENDING: usize = shared::NTP_MAX_CONFIG_SERVERS + 1;

/// One in-flight NTP request waiting for a matching response.
#[derive(Clone, Copy)]
struct PendingQuery {
    /// IP of the server we sent to.
    ip: [u8; 4],
    /// The 8-byte originate token we placed in req[40..48] (nonce ++ ticks_low).
    token: [u8; 8],
    /// Embassy tick counter captured just after send_to returned.
    t1_ticks: u64,
    /// Peer classification for stats reporting.
    source: shared::NtpSource,
    /// Set to true once a valid matching response has been received.
    done: bool,
}

#[allow(clippy::large_stack_frames)]
#[embassy_executor::task]
pub async fn ntp_sync_task(stack: Stack<'static>) {
    let rx_meta = NTP_RX_META.take();
    let tx_meta = NTP_TX_META.take();
    let rx_buffer = NTP_RX_BUFFER.take();
    let tx_buffer = NTP_TX_BUFFER.take();
    let mut first_sync_uptime_s: Option<u64> = None;
    let mut consecutive_failures = 0u8;

    loop {
        stack.wait_config_up().await;

        let (config_peers, dhcp_peer) = ntp_peer_candidates(&stack);
        if config_peers.is_empty() && dhcp_peer.is_none() {
            Timer::after(Duration::from_secs(NTP_RETRY_SECS)).await;
            continue;
        }

        let sleep_secs: u64 = {
            let mut socket = UdpSocket::new(stack, rx_meta, rx_buffer, tx_meta, tx_buffer);
            if socket.bind(NTP_LOCAL_PORT).is_err() {
                println!("ntp: bind failed");
                NTP_RETRY_SECS
            } else {
                // Assert WIFI_PS_NONE directly via the blob symbol immediately
                // before sending queries.  The WiFi driver re-asserts
                // WIFI_PS_MIN_MODEM during the long idle period between polls,
                // so set_power_saving() at connect time doesn't stick.
                // Calling it here ensures the NTP TX frames carry PM=0, which
                // tells the AP to stop buffering unicast replies for us and
                // deliver them at true network RTT instead of the next DTIM
                // beacon (~1024 ms at DTIM=10).
                // ─── Pre-wake ────────────────────────────────────────────────────
                // Setting WIFI_PS_NONE doesn't make the AP immediately deliver
                // buffered unicast traffic.  The AP transitions a STA from
                // "sleeping" to "active" only after it receives a PM=0 frame
                // AND one beacon boundary elapses (~102.4ms).  Fast NTP servers
                // (e.g. a local gateway) respond in ~5ms — well before the AP
                // finishes that transition — so their responses land in the AP
                // buffer and wait for the next DTIM beacon (~1163ms at DTIM=11).
                //
                // Fix: send a pre-wake NTP query to the DHCP gateway and hold a
                // recv_from loop pending for ≥ 1 DTIM period (1250 ms).  A pending
                // recv keeps the WiFi driver active throughout (unlike Timer::after
                // which yields with no I/O and lets the radio sleep).  Any packets
                // received during this window are discarded; Phase 2 handles the
                // real replies.  1250 ms is empirically > the ~1163 ms DTIM period
                // observed at this AP (DTIM=11 × ~102.4 ms/beacon).
                {
                    unsafe extern "C" {
                        fn esp_wifi_set_ps(ps_type: u32) -> i32;
                    }
                    let _ = unsafe { esp_wifi_set_ps(0) };

                    if let Some(gw) = dhcp_peer {
                        let mut wake_req = [0u8; 48];
                        wake_req[0] = 0x1B;
                        let nonce = NTP_QUERY_NONCE
                            .fetch_add(1, Ordering::Relaxed)
                            .to_be_bytes();
                        let ticks_low = (Instant::now().as_ticks() as u32).to_be_bytes();
                        wake_req[40..44].copy_from_slice(&nonce);
                        wake_req[44..48].copy_from_slice(&ticks_low);
                        if socket.send_to(&wake_req, (gw, NTP_PORT)).await.is_ok() {
                            // 1250 ms > one DTIM period; after this the AP considers
                            // the STA active and stops buffering unicast replies.
                            let deadline = Instant::now() + Duration::from_millis(1250);
                            let mut discard_buf = [0u8; 64];
                            loop {
                                let now = Instant::now();
                                if now >= deadline {
                                    break;
                                }
                                let _ = with_timeout(
                                    deadline - now,
                                    socket.recv_from(&mut discard_buf),
                                )
                                .await;
                            }
                        }
                    }
                }

                // ── Phase 1: send all requests back-to-back ───────────────
                //
                // All UDP sends happen in rapid succession while the WiFi radio
                // is guarantee-awake (each send_to flushes the TX FIFO and keeps
                // the radio out of power-save mode).  Waiting for responses
                // one-at-a-time in a sequential loop would let the radio re-enter
                // power-save between send and recv, adding ~1 s of AP-buffering
                // latency (1 DTIM beacon at DTIM=10 × 102.4 ms = 1024 ms).
                let mut pending: heapless::Vec<PendingQuery, NTP_MAX_PENDING> =
                    heapless::Vec::new();

                // Collect all server IPs with their source classification.
                let mut all_servers: heapless::Vec<([u8; 4], shared::NtpSource), NTP_MAX_PENDING> =
                    heapless::Vec::new();
                for ip in config_peers.iter() {
                    let _ = all_servers.push((ip.octets(), shared::NtpSource::Config));
                }
                if let Some(gw) = dhcp_peer {
                    if !config_peers.contains(&gw) {
                        let _ = all_servers.push((gw.octets(), shared::NtpSource::DhcpGateway));
                    }
                }

                for (octets, source) in all_servers.iter().copied() {
                    let server_ip = Ipv4Address::new(octets[0], octets[1], octets[2], octets[3]);
                    let mut req = [0u8; 48];
                    req[0] = 0x1B; // LI=0, VN=3, Mode=3 (SNTPv3 client)
                    let nonce = NTP_QUERY_NONCE
                        .fetch_add(1, Ordering::Relaxed)
                        .to_be_bytes();
                    let ticks_low = (Instant::now().as_ticks() as u32).to_be_bytes();
                    req[40..44].copy_from_slice(&nonce);
                    req[44..48].copy_from_slice(&ticks_low);
                    let token: [u8; 8] = req[40..48].try_into().unwrap();

                    // Capture t1 BEFORE send_to: send_to may block waiting for a TX
                    // metadata slot to be freed by the WiFi blob (ACK of the previous
                    // packet).  Capturing after send_to returns would inflate t1 by the
                    // queue-wait time — exactly the ~1 s "latency" we were measuring.
                    let t1_ticks = Instant::now().as_ticks();
                    println!("ntp: probing {} (source={})", server_ip, source.label());
                    if socket.send_to(&req, (server_ip, NTP_PORT)).await.is_ok() {
                        let _ = pending.push(PendingQuery {
                            ip: octets,
                            token,
                            t1_ticks,
                            source,
                            done: false,
                        });
                    } else {
                        println!("ntp: send failed to {}", server_ip);
                        crate::firmware::status::mark_ntp_peer_query_failed(source, octets);
                    }
                }

                // ── Phase 2: collect responses ────────────────────────────
                //
                // A single recv loop with one shared timeout collects responses
                // from all servers.  Each response is matched to its pending
                // request by the 8-byte originate token (nonce ++ ticks_low).
                // Stale or duplicate packets that don't match any token are
                // silently discarded.
                let mut best: Option<(NtpSample, [u8; 4], shared::NtpSource)> = None;
                let started = Instant::now();
                let timeout = Duration::from_secs(NTP_TIMEOUT_SECS);
                let mut resp = [0u8; 64];

                'recv: loop {
                    let pending_remaining = pending.iter().any(|p| !p.done);
                    if !pending_remaining {
                        break;
                    }
                    let elapsed = Instant::now() - started;
                    if elapsed >= timeout {
                        break;
                    }
                    let remaining = timeout - elapsed;

                    let Ok(Ok((n, _))) = with_timeout(remaining, socket.recv_from(&mut resp)).await
                    else {
                        break;
                    };
                    let recv_ticks = Instant::now().as_ticks();

                    if n < 48 {
                        continue;
                    }

                    // Match response to a pending query by originate timestamp token.
                    let resp_token: [u8; 8] = resp[24..32].try_into().unwrap();
                    let Some(pq) = pending
                        .iter_mut()
                        .find(|p| !p.done && p.token == resp_token)
                    else {
                        // Stale / duplicate / from a different server — discard.
                        continue 'recv;
                    };

                    pq.done = true;
                    let ip = pq.ip;
                    let source = pq.source;
                    let t1_ticks = pq.t1_ticks;

                    let ntp_secs = u32::from_be_bytes([resp[40], resp[41], resp[42], resp[43]]);
                    let ntp_frac = u32::from_be_bytes([resp[44], resp[45], resp[46], resp[47]]);
                    let Some(unix) = ntp_secs.checked_sub(NTP_UNIX_OFFSET) else {
                        continue;
                    };
                    if unix == 0 {
                        continue;
                    }

                    let unix_frac_us = ((ntp_frac as u64 * 1_000_000) >> 32) as u32;
                    let elapsed_us = (recv_ticks
                        .saturating_sub(t1_ticks)
                        .saturating_mul(1_000_000)
                        / embassy_time::TICK_HZ)
                        .min(u32::MAX as u64) as u32;
                    let server_tx_us = unix as i64 * 1_000_000 + unix_frac_us as i64;
                    let t4_est = server_tx_us.saturating_add(elapsed_us as i64 / 2);
                    let offset_us =
                        crate::firmware::status::current_unix_time_micros().map(|local_us| {
                            let delta = t4_est.saturating_sub(local_us as i64);
                            delta.clamp(i32::MIN as i64, i32::MAX as i64) as i32
                        });

                    let sample = NtpSample {
                        unix,
                        unix_frac_us,
                        recv_ticks,
                        stratum: resp[1],
                        latency_ms: elapsed_us.saturating_add(500) / 1_000,
                        jitter_ms: 0,
                        latency_us: elapsed_us,
                        offset_us,
                    };

                    crate::firmware::status::update_ntp_peer_stats(
                        source,
                        ip,
                        sample.stratum,
                        sample.latency_us,
                        sample.offset_us,
                    );
                    let selection = crate::firmware::status::ntp_selection_sample(source, ip)
                        .unwrap_or(shared::NtpSelectionSample {
                            stratum: sample.stratum,
                            latency_ms: sample.latency_ms,
                            jitter_ms: sample.jitter_ms,
                        });
                    let sample = NtpSample {
                        latency_ms: selection.latency_ms,
                        jitter_ms: selection.jitter_ms,
                        ..sample
                    };
                    let choose = match best {
                        None => true,
                        Some((current, _, _)) => should_replace_master(current, sample),
                    };
                    if choose {
                        best = Some((sample, ip, source));
                    }
                }

                // Mark any servers that never responded as failed.
                for pq in pending.iter().filter(|p| !p.done) {
                    crate::firmware::status::mark_ntp_peer_query_failed(pq.source, pq.ip);
                    let ip = Ipv4Address::new(pq.ip[0], pq.ip[1], pq.ip[2], pq.ip[3]);
                    println!("ntp: no response from {}", ip);
                }

                // Mirror stats for the DHCP gateway if it matches a config peer.
                if let Some(gw) = dhcp_peer {
                    if config_peers.contains(&gw) {
                        if let Some((sample, _, source)) = best {
                            if source == shared::NtpSource::Config {
                                crate::firmware::status::update_ntp_peer_stats(
                                    shared::NtpSource::DhcpGateway,
                                    gw.octets(),
                                    sample.stratum,
                                    sample.latency_us,
                                    sample.offset_us,
                                );
                            }
                        }
                    }
                }

                match best {
                    Some((sample, master_ip, source)) => {
                        consecutive_failures = 0;
                        crate::firmware::status::update_ntp_time(
                            sample.unix,
                            sample.unix_frac_us,
                            sample.recv_ticks,
                            sample.latency_us,
                            master_ip,
                            source,
                            sample.stratum,
                        );
                        println!(
                            "ntp: synced unix={} source={} stratum={} latency={}ms",
                            sample.unix,
                            source.label(),
                            sample.stratum,
                            sample.latency_ms
                        );

                        let uptime_s = Instant::now().as_ticks() / embassy_time::TICK_HZ;
                        let baseline = *first_sync_uptime_s.get_or_insert(uptime_s);
                        let age_since_sync_s = uptime_s.saturating_sub(baseline);
                        let next_poll_secs = ntp_poll_interval_secs_after_sync(age_since_sync_s);
                        println!(
                            "ntp: adaptive poll in {}s (age_since_sync={}s)",
                            next_poll_secs, age_since_sync_s
                        );
                        next_poll_secs
                    }
                    None => {
                        consecutive_failures = consecutive_failures.saturating_add(1);
                        println!(
                            "ntp: retry in {}s (consecutive_failures={})",
                            NTP_RETRY_SECS, consecutive_failures
                        );
                        NTP_RETRY_SECS
                    }
                }
            }
        }; // socket dropped here, releasing buffers for next iteration

        Timer::after(Duration::from_secs(sleep_secs)).await;
    }
}
