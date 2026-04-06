// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

//! UDP telemetry sender + LAN server discovery.
//!
//! # Server discovery
//!
//! The firmware locates the LAN server via two mechanisms (first match wins):
//!
//! 1. **Static config** — `UDP_SERVER_IP` set in `config.local.toml` (highest priority).
//! 2. **UDP broadcast beacon** — `udp_discovery_task` listens on `DISCOVERY_PORT`
//!    (47889) for a 12-byte `BRWS` beacon sent by `brewster-server`.
//! 3. **mDNS** — the mDNS task calls `set_discovered_server()` when it sees a
//!    `_brewster._udp.local.` PTR record in an incoming mDNS packet.
//!
//! Both discovery paths write to the shared `DISCOVERED_IP`/`DISCOVERED_PORT`
//! atomics.  The telemetry task checks `server_addr()` before each send.
//!
//! # Telemetry wire format
//!
//! See `server/src/packet.rs` — 33 bytes, magic `b"BREW"` + version byte.

use core::sync::atomic::{AtomicU16, AtomicU32, Ordering};

use embassy_net::Stack;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, Ipv4Address};
use embassy_time::{Duration, Instant, Timer};
use static_cell::ConstStaticCell;

use crate::firmware::{config, status};

// ── Constants ─────────────────────────────────────────────────────────────────

const PACKET_MAGIC: [u8; 4] = *b"BREW";
/// Increment this whenever the wire layout changes so the server can detect
/// format mismatches and drop stale packets cleanly.
const PACKET_VERSION: u8 = 1;
const PACKET_SIZE: usize = 33;
const TEMP_NONE: i16 = i16::MAX;

/// Port the server broadcasts beacons TO; the firmware listens on this port.
pub(super) const DISCOVERY_PORT: u16 = 47889;
/// Magic bytes in the broadcast beacon (b"BRWS" = Brew Server).
pub(super) const BEACON_MAGIC: [u8; 4] = *b"BRWS";
/// Local source port used when sending telemetry packets.
const LOCAL_TELEMETRY_PORT: u16 = 47891;
/// How long to keep sending to a discovered server after the last beacon.
/// Server broadcasts every 5 s; 30 s = 6 missed intervals before giving up.
const DISCOVERY_EXPIRY_S: u32 = 30;

// ── Shared discovery state ────────────────────────────────────────────────────

/// Server IPv4 packed as big-endian u32; 0 = not yet discovered.
static DISCOVERED_IP: AtomicU32 = AtomicU32::new(0);
/// Server telemetry port; 0 = not yet discovered.
static DISCOVERED_PORT: AtomicU16 = AtomicU16::new(0);
/// Device uptime (seconds) at the most recent successful discovery event.
static DISCOVERED_UPTIME_S: AtomicU32 = AtomicU32::new(0);

/// Record a freshly discovered server address and reset the expiry clock.
/// Called on every received beacon (broadcast or mDNS).
pub(super) fn set_discovered_server(ip: [u8; 4], port: u16) {
    let now_s = (Instant::now().as_ticks() / embassy_time::TICK_HZ) as u32;
    DISCOVERED_IP.store(u32::from_be_bytes(ip), Ordering::Relaxed);
    DISCOVERED_PORT.store(port, Ordering::Relaxed);
    DISCOVERED_UPTIME_S.store(now_s, Ordering::Relaxed);
}

/// Resolve the server address to send telemetry to.
///
/// Returns `None` when:
/// - No address has been configured or discovered yet.
/// - The dynamic discovery has expired (server silent for >30 s).
///   Static `UDP_SERVER_IP` config is exempt from expiry.
fn server_addr() -> Option<([u8; 4], u16)> {
    // Static config takes priority and never expires.
    if let Some(s) = config::UDP_SERVER_IP_CONFIG.filter(|s| !s.is_empty()) {
        if let Some(octets) = parse_ipv4_octets(s) {
            return Some((octets, config::udp_server_port()));
        }
    }
    // Dynamic discovery — validate and check expiry.
    let ip_raw = DISCOVERED_IP.load(Ordering::Relaxed);
    let port = DISCOVERED_PORT.load(Ordering::Relaxed);
    if ip_raw == 0 || port == 0 {
        return None;
    }
    let discovered_s = DISCOVERED_UPTIME_S.load(Ordering::Relaxed);
    let now_s = (Instant::now().as_ticks() / embassy_time::TICK_HZ) as u32;
    if now_s.wrapping_sub(discovered_s) > DISCOVERY_EXPIRY_S {
        return None; // Server has been silent — stop sending until rediscovered.
    }
    Some((ip_raw.to_be_bytes(), port))
}

// ── Static socket buffers ─────────────────────────────────────────────────────

static TELEMETRY_RX_META: ConstStaticCell<[PacketMetadata; 1]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; 1]);
static TELEMETRY_TX_META: ConstStaticCell<[PacketMetadata; 1]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; 1]);
static TELEMETRY_RX_BUF: ConstStaticCell<[u8; 32]> = ConstStaticCell::new([0; 32]);
static TELEMETRY_TX_BUF: ConstStaticCell<[u8; 64]> = ConstStaticCell::new([0; 64]);

static DISC_RX_META: ConstStaticCell<[PacketMetadata; 1]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; 1]);
static DISC_TX_META: ConstStaticCell<[PacketMetadata; 1]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; 1]);
static DISC_RX_BUF: ConstStaticCell<[u8; 32]> = ConstStaticCell::new([0; 32]);
static DISC_TX_BUF: ConstStaticCell<[u8; 32]> = ConstStaticCell::new([0; 32]);

// ── Telemetry seq counter ─────────────────────────────────────────────────────

static UDP_SEQ: AtomicU32 = AtomicU32::new(0);
/// Packets successfully sent to the LAN server since boot.
static TELEMETRY_SENT: AtomicU32 = AtomicU32::new(0);
/// Packets that failed to send (socket error) since boot.
static TELEMETRY_FAILED: AtomicU32 = AtomicU32::new(0);
/// Set when local history is cleared; consumed (cleared) by the next telemetry
/// packet so the server can auto-clear its own store.
static HISTORY_CLEAR_PENDING: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Signal that the device history has been cleared. The next telemetry packet
/// will carry a flag (bit 3 of the flags byte) that tells the server to clear
/// its own ring buffer, keeping both sides in sync.
pub(super) fn set_history_clear_pending() {
    HISTORY_CLEAR_PENDING.store(true, Ordering::Relaxed);
}

// ── Discovery task ────────────────────────────────────────────────────────────

/// Listen on `DISCOVERY_PORT` for UDP broadcast beacons from `brewster-server`.
///
/// Beacon format (12 bytes):
/// ```text
/// [0..4]   magic: b"BRWS"
/// [4..8]   server IPv4 (big-endian)
/// [8..10]  telemetry UDP port (little-endian)
/// [10..12] HTTP port (little-endian, informational)
/// ```
#[embassy_executor::task]
pub async fn udp_discovery_task(stack: Stack<'static>) {
    // Wait for DHCP before opening the socket.
    loop {
        if status::ip_octets().is_some() {
            break;
        }
        Timer::after(Duration::from_secs(1)).await;
    }

    let mut socket = UdpSocket::new(
        stack,
        DISC_RX_META.take(),
        DISC_RX_BUF.take(),
        DISC_TX_META.take(),
        DISC_TX_BUF.take(),
    );

    if let Err(e) = socket.bind(DISCOVERY_PORT) {
        esp_println::println!("udp discovery: bind failed: {:?}", e);
        return;
    }

    esp_println::println!("udp discovery: listening on port {}", DISCOVERY_PORT);

    let mut buf = [0u8; 16];
    loop {
        let Ok((len, _peer)) = socket.recv_from(&mut buf).await else {
            continue;
        };
        if len < 12 {
            continue;
        }
        if buf[0..4] != BEACON_MAGIC {
            continue;
        }
        let ip = [buf[4], buf[5], buf[6], buf[7]];
        let port = u16::from_le_bytes([buf[8], buf[9]]);
        if ip == [0u8; 4] || port == 0 {
            continue;
        }
        let current = DISCOVERED_IP.load(Ordering::Relaxed);
        let new_ip = u32::from_be_bytes(ip);
        if current != new_ip {
            esp_println::println!(
                "udp discovery: server found at {}.{}.{}.{}:{}",
                ip[0], ip[1], ip[2], ip[3], port
            );
        }
        set_discovered_server(ip, port);
    }
}

// ── Telemetry sender task ─────────────────────────────────────────────────────

/// Send a 32-byte telemetry packet to the LAN server every second.
///
/// Behavior:
/// - Opens the socket immediately after DHCP so the first send is instant.
/// - Calls `server_addr()` on every tick; only sends when it returns `Some`.
/// - Logs transitions between "server known" and "server lost" states.
/// - Static `UDP_SERVER_IP` config is always live; dynamic discovery expires
///   after `DISCOVERY_EXPIRY_S` seconds without a beacon.
#[embassy_executor::task]
pub async fn udp_telemetry_task(stack: Stack<'static>) {
    // Wait for DHCP before opening the socket.
    loop {
        if status::ip_octets().is_some() {
            break;
        }
        Timer::after(Duration::from_secs(1)).await;
    }

    // Open the socket now so we can send immediately when a server is discovered.
    let mut socket = UdpSocket::new(
        stack,
        TELEMETRY_RX_META.take(),
        TELEMETRY_RX_BUF.take(),
        TELEMETRY_TX_META.take(),
        TELEMETRY_TX_BUF.take(),
    );

    if let Err(e) = socket.bind(LOCAL_TELEMETRY_PORT) {
        esp_println::println!("udp telemetry: bind failed: {:?}", e);
        return;
    }

    esp_println::println!("udp telemetry: socket ready, awaiting server discovery");

    let mut last_known: Option<([u8; 4], u16)> = None;

    loop {
        Timer::after(Duration::from_secs(1)).await;

        let addr = server_addr();

        // Log state transitions: discovered / lost.
        match (last_known, addr) {
            (None, Some((ip, port))) => {
                esp_println::println!(
                    "udp telemetry: server at {}.{}.{}.{}:{} — sending",
                    ip[0], ip[1], ip[2], ip[3], port
                );
            }
            (Some(_), None) => {
                esp_println::println!("udp telemetry: server lost — waiting for rediscovery");
            }
            _ => {}
        }
        last_known = addr;

        let Some((server_ip_octets, server_port)) = addr else {
            continue; // Server not known — do not send.
        };

        let server_ip = Ipv4Address::new(
            server_ip_octets[0],
            server_ip_octets[1],
            server_ip_octets[2],
            server_ip_octets[3],
        );

        let buf = build_packet();
        match socket
            .send_to(&buf, (IpAddress::Ipv4(server_ip), server_port))
            .await
        {
            Ok(()) => {
                let seq = TELEMETRY_SENT.fetch_add(1, Ordering::Relaxed) + 1;
                status::udp_send_notify();
                esp_println::println!(
                    "udp telemetry: sent packet #{} to {}.{}.{}.{}:{}",
                    seq,
                    server_ip_octets[0],
                    server_ip_octets[1],
                    server_ip_octets[2],
                    server_ip_octets[3],
                    server_port
                );
            }
            Err(e) => {
                TELEMETRY_FAILED.fetch_add(1, Ordering::Relaxed);
                esp_println::println!("udp telemetry: send error: {:?}", e);
            }
        }
    }
}

// ── Packet builder ────────────────────────────────────────────────────────────

fn build_packet() -> [u8; PACKET_SIZE] {
    let seq = UDP_SEQ.fetch_add(1, Ordering::Relaxed);
    let uptime_s = (Instant::now().as_ticks() / embassy_time::TICK_HZ) as u32;

    let snap = status::metrics_snapshot();
    let ip = if snap.ip_valid { snap.ip_octets } else { [0u8; 4] };
    let sensor_count = config::SENSORS.len().min(3) as u8;

    let encode_temp = |centi: i32| -> i16 {
        if centi == status::UNKNOWN_TEMPERATURE_CENTI {
            TEMP_NONE
        } else {
            centi.clamp(i16::MIN as i32, (i16::MAX - 1) as i32) as i16
        }
    };

    let temp0 = encode_temp(snap.temp_centi);
    let temp1 = encode_temp(status::sensor_temp_centi(1));
    let temp2 = encode_temp(status::sensor_temp_centi(2));
    let target_centi = (snap.target_c * 100.0) as i16;
    let output_pct = (snap.pid_deci / 10) as u8;
    let history_clear = HISTORY_CLEAR_PENDING.swap(false, Ordering::Relaxed);
    let flags = (snap.relay_on as u8)
        | ((snap.collection_enabled as u8) << 1)
        | ((snap.ntp_synced as u8) << 2)
        | ((history_clear as u8) << 3);

    let mut buf = [0u8; PACKET_SIZE];
    buf[0..4].copy_from_slice(&PACKET_MAGIC);
    buf[4] = PACKET_VERSION;
    buf[5..9].copy_from_slice(&seq.to_le_bytes());
    buf[9..13].copy_from_slice(&uptime_s.to_le_bytes());
    buf[13..15].copy_from_slice(&temp0.to_le_bytes());
    buf[15..17].copy_from_slice(&temp1.to_le_bytes());
    buf[17..19].copy_from_slice(&temp2.to_le_bytes());
    buf[19..21].copy_from_slice(&target_centi.to_le_bytes());
    buf[21] = output_pct;
    buf[22] = flags;
    buf[23] = snap.pid_window_step;
    buf[24] = snap.pid_on_steps;
    buf[25] = snap.sensor_status_code;
    buf[26] = status::sensor_status(1);
    buf[27] = status::sensor_status(2);
    buf[28..32].copy_from_slice(&ip);
    buf[32] = sensor_count;
    buf
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub(super) fn telemetry_stats() -> (u32, u32) {
    (
        TELEMETRY_SENT.load(Ordering::Relaxed),
        TELEMETRY_FAILED.load(Ordering::Relaxed),
    )
}

pub(super) fn discovered_server_ip_octets() -> Option<[u8; 4]> {
    let ip = DISCOVERED_IP.load(Ordering::Relaxed);
    if ip != 0 { Some(ip.to_be_bytes()) } else { None }
}

fn parse_ipv4_octets(s: &str) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut count = 0usize;
    for part in s.split('.') {
        if count >= 4 {
            return None;
        }
        octets[count] = part.parse::<u8>().ok()?;
        count += 1;
    }
    if count == 4 { Some(octets) } else { None }
}
