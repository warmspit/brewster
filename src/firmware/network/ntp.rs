use core::sync::atomic::{AtomicU32, Ordering};

use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{Ipv4Address, Stack};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_println::println;
use static_cell::ConstStaticCell;

use crate::firmware::shared;

const NTP_PORT: u16 = 123;
const NTP_LOCAL_PORT: u16 = 1125;
const NTP_SYNC_INTERVAL_SECS: u64 = 3_600;
const NTP_RETRY_SECS: u64 = 60;
const NTP_TIMEOUT_SECS: u64 = 5;
const NTP_MIN_POLL_SECS: u64 = 60;
const NTP_POLL_RAMP_DURATION_SECS: u64 = 3_600;
const NTP_IBURST_PROBES: u16 = 500;
const NTP_IBURST_INTERVAL_SECS: u64 = 1;
const NTP_REBURST_PROBES: u16 = 4;
const NTP_REBURST_AFTER_FAILURES: u8 = 3;
const NTP_UNIX_OFFSET: u32 = 2_208_988_800;
const NTP_SERVERS_CONFIG: Option<&str> = option_env!("NTP_SERVERS");
const NTP_SERVER_CONFIG: Option<&str> = option_env!("NTP_SERVER");

static NTP_RX_META: ConstStaticCell<[PacketMetadata; 1]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; 1]);
static NTP_TX_META: ConstStaticCell<[PacketMetadata; 1]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; 1]);
static NTP_RX_BUFFER: ConstStaticCell<[u8; 64]> = ConstStaticCell::new([0; 64]);
static NTP_TX_BUFFER: ConstStaticCell<[u8; 64]> = ConstStaticCell::new([0; 64]);
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

#[allow(
    clippy::large_stack_frames,
    reason = "single-shot NTP query uses fixed request/response packet buffers"
)]
async fn query_ntp_once(socket: &mut UdpSocket<'_>, server_ip: Ipv4Address) -> Option<NtpSample> {
    let mut req = [0u8; 48];
    req[0] = 0x1B; // LI=0, VN=3, Mode=3 (SNTPv3 client)
    let nonce = NTP_QUERY_NONCE
        .fetch_add(1, Ordering::Relaxed)
        .to_be_bytes();
    let ticks_low = (Instant::now().as_ticks() as u32).to_be_bytes();
    // Originate token must be unique per request to avoid accepting delayed replies
    // from other peers while reusing a single UDP socket.
    req[40..44].copy_from_slice(&nonce);
    req[44..48].copy_from_slice(&ticks_low);

    if socket.send_to(&req, (server_ip, NTP_PORT)).await.is_err() {
        return None;
    }
    // Measure network reply time only; send_to may include one-time ARP/link setup.
    let started = Instant::now();
    let started_ticks = started.as_ticks();

    let mut resp = [0u8; 64];
    let timeout = Duration::from_secs(NTP_TIMEOUT_SECS);

    loop {
        let now = Instant::now();
        let elapsed = now - started;
        if elapsed >= timeout {
            return None;
        }
        let remaining = timeout - elapsed;

        let Ok(Ok((n, _))) = with_timeout(remaining, socket.recv_from(&mut resp)).await else {
            return None;
        };
        let recv_ticks = Instant::now().as_ticks();

        if n < 48 {
            continue;
        }

        // NTP response must echo client transmit timestamp in originate timestamp.
        if resp[24..32] != req[40..48] {
            continue;
        }

        let ntp_secs = u32::from_be_bytes([resp[40], resp[41], resp[42], resp[43]]);
        let ntp_frac = u32::from_be_bytes([resp[44], resp[45], resp[46], resp[47]]);
        let unix = ntp_secs.checked_sub(NTP_UNIX_OFFSET)?;
        if unix == 0 {
            return None;
        }

        let unix_frac_us = ((ntp_frac as u64 * 1_000_000) >> 32) as u32;
        let elapsed_us = ((recv_ticks - started_ticks).saturating_mul(1_000_000)
            / embassy_time::TICK_HZ)
            .min(u32::MAX as u64) as u32;
        let server_tx_us = unix as i64 * 1_000_000 + unix_frac_us as i64;
        let offset_us = crate::firmware::status::current_unix_time_micros().map(|local_us| {
            let delta = server_tx_us.saturating_sub(local_us as i64);
            delta.clamp(i32::MIN as i64, i32::MAX as i64) as i32
        });

        return Some(NtpSample {
            unix,
            unix_frac_us,
            recv_ticks,
            stratum: resp[1],
            latency_ms: elapsed_us.saturating_add(500) / 1_000,
            jitter_ms: 0,
            latency_us: elapsed_us,
            offset_us,
        });
    }
}

#[allow(clippy::large_stack_frames)]
#[embassy_executor::task]
pub async fn ntp_sync_task(stack: Stack<'static>) {
    let rx_meta = NTP_RX_META.take();
    let tx_meta = NTP_TX_META.take();
    let rx_buffer = NTP_RX_BUFFER.take();
    let tx_buffer = NTP_TX_BUFFER.take();
    let mut iburst_remaining = NTP_IBURST_PROBES;
    let mut first_sync_uptime_s: Option<u64> = None;
    let mut had_successful_sync = false;
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
                let mut best: Option<(NtpSample, [u8; 4], shared::NtpSource)> = None;

                for ip in config_peers.iter().copied() {
                    println!("ntp: probing {} (source=config)", ip);
                    match query_ntp_once(&mut socket, ip).await {
                        Some(sample) => {
                            crate::firmware::status::update_ntp_peer_stats(
                                shared::NtpSource::Config,
                                ip.octets(),
                                sample.stratum,
                                sample.latency_us,
                                sample.offset_us,
                            );
                            let selection = crate::firmware::status::ntp_selection_sample(
                                shared::NtpSource::Config,
                                ip.octets(),
                            )
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
                                best = Some((sample, ip.octets(), shared::NtpSource::Config));
                            }
                        }
                        None => {
                            crate::firmware::status::mark_ntp_peer_query_failed(
                                shared::NtpSource::Config,
                                ip.octets(),
                            );
                            println!("ntp: no response from {}", ip);
                        }
                    }
                }

                if let Some(ip) = dhcp_peer {
                    if !config_peers.contains(&ip) {
                        println!("ntp: probing {} (source=dhcp_gateway)", ip);
                        match query_ntp_once(&mut socket, ip).await {
                            Some(sample) => {
                                crate::firmware::status::update_ntp_peer_stats(
                                    shared::NtpSource::DhcpGateway,
                                    ip.octets(),
                                    sample.stratum,
                                    sample.latency_us,
                                    sample.offset_us,
                                );
                                let selection = crate::firmware::status::ntp_selection_sample(
                                    shared::NtpSource::DhcpGateway,
                                    ip.octets(),
                                )
                                .unwrap_or(
                                    shared::NtpSelectionSample {
                                        stratum: sample.stratum,
                                        latency_ms: sample.latency_ms,
                                        jitter_ms: sample.jitter_ms,
                                    },
                                );
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
                                    best =
                                        Some((sample, ip.octets(), shared::NtpSource::DhcpGateway));
                                }
                            }
                            None => {
                                crate::firmware::status::mark_ntp_peer_query_failed(
                                    shared::NtpSource::DhcpGateway,
                                    ip.octets(),
                                );
                                println!("ntp: no response from {}", ip);
                            }
                        }
                    } else if let Some((sample, _, source)) = best {
                        // Config and DHCP peer are the same endpoint; mirror stats for DHCP view.
                        if source == shared::NtpSource::Config {
                            crate::firmware::status::update_ntp_peer_stats(
                                shared::NtpSource::DhcpGateway,
                                ip.octets(),
                                sample.stratum,
                                sample.latency_us,
                                sample.offset_us,
                            );
                        }
                    }
                }

                match best {
                    Some((sample, master_ip, source)) => {
                        consecutive_failures = 0;
                        had_successful_sync = true;
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

                        if iburst_remaining > 0 {
                            let completed = NTP_IBURST_PROBES.saturating_sub(iburst_remaining) + 1;
                            iburst_remaining = iburst_remaining.saturating_sub(1);
                            println!(
                                "ntp: iburst sample {}/{}; next poll in {}s (remaining={})",
                                completed,
                                NTP_IBURST_PROBES,
                                NTP_IBURST_INTERVAL_SECS,
                                iburst_remaining
                            );
                            NTP_IBURST_INTERVAL_SECS
                        } else {
                            let uptime_s = Instant::now().as_ticks() / embassy_time::TICK_HZ;
                            let baseline = *first_sync_uptime_s.get_or_insert(uptime_s);
                            let age_since_sync_s = uptime_s.saturating_sub(baseline);
                            let next_poll_secs =
                                ntp_poll_interval_secs_after_sync(age_since_sync_s);
                            println!(
                                "ntp: adaptive poll in {}s (age_since_sync={}s)",
                                next_poll_secs, age_since_sync_s
                            );
                            next_poll_secs
                        }
                    }
                    None => {
                        consecutive_failures = consecutive_failures.saturating_add(1);

                        if iburst_remaining > 0 {
                            let completed = NTP_IBURST_PROBES.saturating_sub(iburst_remaining) + 1;
                            iburst_remaining = iburst_remaining.saturating_sub(1);
                            println!(
                                "ntp: iburst miss {}/{}; retry in {}s (remaining={})",
                                completed,
                                NTP_IBURST_PROBES,
                                NTP_IBURST_INTERVAL_SECS,
                                iburst_remaining
                            );
                            NTP_IBURST_INTERVAL_SECS
                        } else if had_successful_sync
                            && consecutive_failures >= NTP_REBURST_AFTER_FAILURES
                        {
                            iburst_remaining = NTP_REBURST_PROBES;
                            first_sync_uptime_s = None;
                            had_successful_sync = false;
                            println!(
                                "ntp: sync degraded ({} consecutive failures); entering re-burst ({} probes every {}s)",
                                consecutive_failures, NTP_REBURST_PROBES, NTP_IBURST_INTERVAL_SECS
                            );
                            NTP_IBURST_INTERVAL_SECS
                        } else {
                            println!(
                                "ntp: retry in {}s (consecutive_failures={})",
                                NTP_RETRY_SECS, consecutive_failures
                            );
                            NTP_RETRY_SECS
                        }
                    }
                }
            }
        }; // socket dropped here, releasing buffers for next iteration

        Timer::after(Duration::from_secs(sleep_secs)).await;
    }
}
