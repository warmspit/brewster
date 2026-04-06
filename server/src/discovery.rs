// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

//! LAN discovery — the server announces its presence via two mechanisms so
//! the firmware can find it without a hardcoded IP:
//!
//! 1. **UDP broadcast beacon** — sent every 5 s to `255.255.255.255:DISCOVERY_PORT`
//!    (port 47889).  Payload is 12 bytes:
//!    ```text
//!    [0..4]   magic: b"BRWS"
//!    [4..8]   server IPv4 (big-endian octets)
//!    [8..10]  telemetry UDP port (little-endian)
//!    [10..12] HTTP port (little-endian)
//!    ```
//!
//! 2. **mDNS announcement** — gratuitous response sent every 5 s to
//!    `224.0.0.251:5353` advertising `_brewster._udp.local.`.
//!    Includes PTR → TXT → SRV → A chain so any mDNS listener can
//!    extract the server IP and telemetry port.

use std::net::{IpAddr, UdpSocket as StdUdpSocket};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::sleep;

/// Port the firmware listens on for broadcast beacons.
pub const DISCOVERY_PORT: u16 = 47889;
const MDNS_MULTICAST_ADDR: &str = "224.0.0.251:5353";
const BEACON_MAGIC: [u8; 4] = *b"BRWS";
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(5);
const MDNS_TTL: u32 = 120;

/// Announce the server's presence via UDP broadcast beacon and mDNS gratuitous response.
/// Strings are pre-computed before the loop; `local_ipv4()` is called once per interval.
/// Never returns.
pub async fn run(udp_port: u16, http_port: u16, device_name: String) {
    // Pre-compute everything that doesn't change between announcements.
    let instance_name = format!("{device_name}-server");
    let txt_record = format!("udp={udp_port}");
    let broadcast_dest = format!("255.255.255.255:{DISCOVERY_PORT}");

    let broadcast_sock = match UdpSocket::bind("0.0.0.0:0").await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("discovery: broadcast bind failed: {e}");
            return;
        }
    };
    if let Err(e) = broadcast_sock.set_broadcast(true) {
        tracing::error!("discovery: SO_BROADCAST failed: {e}");
        return;
    }

    let mdns_sock = match UdpSocket::bind("0.0.0.0:0").await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("discovery: mDNS socket bind failed: {e}");
            return;
        }
    };

    tracing::info!(
        "discovery: announcing every {interval}s \u{2014} broadcast:{broadcast_dest}  \
         mDNS:{instance_name}._brewster._udp.local.",
        interval = ANNOUNCE_INTERVAL.as_secs(),
    );

    // Buffer is reused across ticks; no stack zeroing inside the loop.
    let mut mdns_buf = [0u8; 512];
    loop {
        if let Some(ip) = local_ipv4() {
            // UDP broadcast beacon.
            let beacon = build_beacon(ip, udp_port, http_port);
            if let Err(e) = broadcast_sock.send_to(&beacon, &broadcast_dest).await {
                tracing::warn!("discovery: broadcast send failed: {e}");
            }

            // mDNS gratuitous announcement.
            if let Some(n) = build_mdns_packet(
                ip,
                udp_port,
                instance_name.as_bytes(),
                txt_record.as_bytes(),
                &mut mdns_buf,
            ) {
                if let Err(e) = mdns_sock.send_to(&mdns_buf[..n], MDNS_MULTICAST_ADDR).await {
                    tracing::warn!("discovery: mDNS send failed: {e}");
                }
            }
        }
        sleep(ANNOUNCE_INTERVAL).await;
    }
}

// ── UDP broadcast ─────────────────────────────────────────────────────────────

fn build_beacon(ip: [u8; 4], telemetry_port: u16, http_port: u16) -> [u8; 12] {
    let mut buf = [0u8; 12];
    buf[0..4].copy_from_slice(&BEACON_MAGIC);
    buf[4..8].copy_from_slice(&ip);
    buf[8..10].copy_from_slice(&telemetry_port.to_le_bytes());
    buf[10..12].copy_from_slice(&http_port.to_le_bytes());
    buf
}

// ── mDNS gratuitous announcement ─────────────────────────────────────────────

/// Build a gratuitous mDNS response advertising `_brewster._udp.local.`.
///
/// The instance and host names are derived from `device_name`:
/// `{device_name}-server._brewster._udp.local.` / `{device_name}-server.local.`
///
/// Record structure:
/// ```text
/// Answer:     PTR  _brewster._udp.local. → {device_name}-server._brewster._udp.local.
/// Answer:     TXT  {device_name}-server._brewster._udp.local. → "udp=<port>"
/// Additional: SRV  {device_name}-server._brewster._udp.local. → port & {device_name}-server.local.
/// Additional: A    {device_name}-server.local. → <server_ip>
/// ```
fn build_mdns_packet(ip: [u8; 4], telemetry_port: u16, instance_name: &[u8], txt_record: &[u8], out: &mut [u8]) -> Option<usize> {
    let mut i = 0usize;

    // Service, instance, and host labels (no DNS compression — simple & correct).
    let service_labels: &[&[u8]] = &[b"_brewster", b"_udp", b"local"];
    let instance_labels: &[&[u8]] = &[instance_name, b"_brewster", b"_udp", b"local"];
    let host_labels: &[&[u8]] = &[instance_name, b"local"];

    // ── DNS header ────────────────────────────────────────────────────────────
    // ID=0, QR=1 (response), AA=1 (authoritative), no other flags
    // QD=0, AN=2 (PTR+TXT), NS=0, AR=2 (SRV+A)
    put_u16(out, &mut i, 0x0000)?;  // ID
    put_u16(out, &mut i, 0x8400)?;  // Flags: Response + Authoritative
    put_u16(out, &mut i, 0x0000)?;  // QDCOUNT
    put_u16(out, &mut i, 0x0002)?;  // ANCOUNT
    put_u16(out, &mut i, 0x0000)?;  // NSCOUNT
    put_u16(out, &mut i, 0x0002)?;  // ARCOUNT

    // ── Answer 1: PTR _brewster._udp.local. → {name}-server._brewster._udp.local. ──
    put_name(out, &mut i, service_labels)?;
    put_u16(out, &mut i, 12)?;      // TYPE PTR
    put_u16(out, &mut i, 0x0001)?;  // CLASS IN
    put_u32(out, &mut i, MDNS_TTL)?;
    let rdlen_pos = i;
    put_u16(out, &mut i, 0)?;       // placeholder rdlen
    let rdata_start = i;
    put_name(out, &mut i, instance_labels)?;
    let rdata_len = (i - rdata_start) as u16;
    out[rdlen_pos..rdlen_pos + 2].copy_from_slice(&rdata_len.to_be_bytes());

    // ── Answer 2: TXT {name}-server._brewster._udp.local. ──
    put_name(out, &mut i, instance_labels)?;
    put_u16(out, &mut i, 16)?;      // TYPE TXT
    put_u16(out, &mut i, 0x8001)?;  // CLASS IN, cache-flush
    put_u32(out, &mut i, MDNS_TTL)?;
    put_u16(out, &mut i, (1 + txt_record.len()) as u16)?; // rdlen
    *out.get_mut(i)? = txt_record.len() as u8; i += 1; // TXT string length prefix
    let end = i + txt_record.len();
    if end > out.len() { return None; }
    out[i..end].copy_from_slice(txt_record);
    i = end;

    // ── Additional 1: SRV {name}-server._brewster._udp.local. ──
    put_name(out, &mut i, instance_labels)?;
    put_u16(out, &mut i, 33)?;      // TYPE SRV
    put_u16(out, &mut i, 0x8001)?;  // CLASS IN, cache-flush
    put_u32(out, &mut i, MDNS_TTL)?;
    let rdlen_pos = i;
    put_u16(out, &mut i, 0)?;
    let rdata_start = i;
    put_u16(out, &mut i, 0)?;               // priority
    put_u16(out, &mut i, 0)?;               // weight
    put_u16(out, &mut i, telemetry_port)?;  // port
    put_name(out, &mut i, host_labels)?;    // target
    let rdata_len = (i - rdata_start) as u16;
    out[rdlen_pos..rdlen_pos + 2].copy_from_slice(&rdata_len.to_be_bytes());

    // ── Additional 2: A {name}-server.local. → <ip> ──
    put_name(out, &mut i, host_labels)?;
    put_u16(out, &mut i, 1)?;       // TYPE A
    put_u16(out, &mut i, 0x8001)?;  // CLASS IN, cache-flush
    put_u32(out, &mut i, MDNS_TTL)?;
    put_u16(out, &mut i, 4)?;       // rdlen
    let end = i + 4;
    if end > out.len() { return None; }
    out[i..end].copy_from_slice(&ip);
    i = end;

    Some(i)
}

// ── DNS wire-format helpers ───────────────────────────────────────────────────

fn put_u16(buf: &mut [u8], i: &mut usize, v: u16) -> Option<()> {
    let end = i.checked_add(2)?;
    buf.get_mut(*i..end)?.copy_from_slice(&v.to_be_bytes());
    *i = end;
    Some(())
}

fn put_u32(buf: &mut [u8], i: &mut usize, v: u32) -> Option<()> {
    let end = i.checked_add(4)?;
    buf.get_mut(*i..end)?.copy_from_slice(&v.to_be_bytes());
    *i = end;
    Some(())
}

fn put_name(buf: &mut [u8], i: &mut usize, labels: &[&[u8]]) -> Option<()> {
    for label in labels {
        let len = label.len();
        *buf.get_mut(*i)? = len as u8;
        *i += 1;
        let end = i.checked_add(len)?;
        buf.get_mut(*i..end)?.copy_from_slice(label);
        *i = end;
    }
    *buf.get_mut(*i)? = 0; // root terminator
    *i += 1;
    Some(())
}

// ── Local IP detection ────────────────────────────────────────────────────────

/// Detect the preferred outbound IPv4 address without sending any traffic.
fn local_ipv4() -> Option<[u8; 4]> {
    let sock = StdUdpSocket::bind("0.0.0.0:0").ok()?;
    // "Connecting" a UDP socket doesn't send anything; it just sets the
    // kernel routing table entry so we can query the local address.
    sock.connect("8.8.8.8:80").ok()?;
    match sock.local_addr().ok()?.ip() {
        IpAddr::V4(addr) => Some(addr.octets()),
        _ => None,
    }
}
