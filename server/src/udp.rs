// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

//! UDP telemetry receiver — decodes incoming packets and inserts into the store.
//!
//! # Hostname-based device filtering
//!
//! Each packet carries the sender's hostname (max 20 bytes, null-padded).  The
//! receiver tracks the *current* accepted hostname:
//!
//! - On the **first** packet (hostname slot all-zero), the hostname is recorded.
//! - When a packet arrives with a **new** hostname, it is accepted, the current
//!   hostname is updated, and the store is cleared so stale history from the
//!   previous device is discarded.
//! - Any packet whose hostname does **not** match the current hostname is
//!   **silently dropped** — this prevents stray devices from polluting the store.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::RwLock;

use tokio::net::UdpSocket;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::packet;
use crate::persist;
use crate::store::Store;

/// Hostname of the currently accepted device; all-zero = not yet established.
static CURRENT_HOSTNAME: RwLock<[u8; 20]> = RwLock::new([0u8; 20]);

fn hostname_display(bytes: &[u8; 20]) -> &str {
    std::str::from_utf8(bytes)
        .unwrap_or("<invalid-utf8>")
        .trim_end_matches('\0')
}

/// Receive telemetry packets forever, inserting each valid one into `store`.
/// Sends `()` on `notify` after each successfully decoded packet.
/// Appends each stored record immediately to `data_path`.
pub async fn run(sock: UdpSocket, store: Store, notify: broadcast::Sender<()>, data_path: PathBuf) {
    let mut buf = [0u8; 64];
    let mut last_bad: Option<(SocketAddr, usize, Option<u8>)> = None;
    loop {
        let (len, peer) = match sock.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) => {
                error!("udp recv error: {e}");
                continue;
            }
        };
        let pkt = match packet::Packet::decode(&buf[..len]) {
            Some(p) => p,
            None => {
                let version: Option<u8> = if len > 4 && &buf[0..4] == packet::PACKET_MAGIC {
                    Some(buf[4])
                } else {
                    None
                };
                let key = (peer, len, version);
                if last_bad.as_ref() != Some(&key) {
                    match version {
                        Some(v) => warn!(
                            "udp: bad packet from {peer} ({len} bytes version={v} want={})",
                            packet::PACKET_VERSION
                        ),
                        None => warn!("udp: bad packet from {peer} ({len} bytes)"),
                    }
                    last_bad = Some(key);
                }
                continue;
            }
        };
        last_bad = None;

        // --- hostname check ---
        let current = *CURRENT_HOSTNAME.read().unwrap();
        if current == [0u8; 20] {
            // First packet: record this device's hostname.
            *CURRENT_HOSTNAME.write().unwrap() = pkt.hostname;
            info!(
                "udp: accepted device '{}' from {peer}",
                hostname_display(&pkt.hostname)
            );
        } else if pkt.hostname != current {
            // Different hostname — new device takes over.
            info!(
                "udp: device changed '{}' -> '{}' from {peer}; clearing store",
                hostname_display(&current),
                hostname_display(&pkt.hostname)
            );
            *CURRENT_HOSTNAME.write().unwrap() = pkt.hostname;
            store.clear();
            persist::clear(&data_path);
        } else {
            // Hostname matches — drop packets from other devices silently.
            // (Nothing to do; fall through to insert.)
        }

        if pkt.history_clear {
            store.clear();
            persist::clear(&data_path);
        }
        if let Some(record) = store.insert(pkt) {
            persist::append(&record, &data_path);
        }
        let _ = notify.send(());
    }
}
