// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

//! UDP telemetry receiver — decodes incoming packets and inserts into the store.
//!
//! # Nonce-based session filtering
//!
//! Each ESP32 generates a random 32-bit nonce at boot and includes it in every
//! packet.  The receiver tracks the *current* accepted nonce:
//!
//! - On the **first** packet (`current == 0`), the nonce is accepted and recorded.
//! - When a packet arrives with a **new** nonce, it is accepted and the current
//!   nonce is updated (the new device session takes over; the store is cleared so
//!   stale history from the previous session is discarded).
//! - Any packet whose nonce does **not** match the current nonce is **silently
//!   dropped** — this prevents stale or stray devices from polluting the store.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use tokio::net::UdpSocket;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::packet;
use crate::persist;
use crate::store::Store;

/// Nonce of the currently accepted ESP32 session; 0 = not yet established.
static CURRENT_NONCE: AtomicU32 = AtomicU32::new(0);

/// Receive telemetry packets forever, inserting each valid one into `store`.
/// Sends `()` on `notify` after each successfully decoded packet.
/// Appends each stored record immediately to `data_path`.
pub async fn run(sock: UdpSocket, store: Store, notify: broadcast::Sender<()>, data_path: PathBuf) {
    let mut buf = [0u8; 64];
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
                warn!("udp: bad packet from {peer} ({len} bytes)");
                continue;
            }
        };

        // --- nonce check ---
        let current = CURRENT_NONCE.load(Ordering::Relaxed);
        if current == 0 {
            // First packet ever: accept and record this device session.
            CURRENT_NONCE.store(pkt.nonce, Ordering::Relaxed);
            info!("udp: new session nonce {:#010x} from {peer}", pkt.nonce);
        } else if pkt.nonce != current {
            // A different nonce arrived — new device session takes over.
            info!(
                "udp: nonce changed {:#010x} -> {:#010x} from {peer}; clearing store",
                current, pkt.nonce
            );
            CURRENT_NONCE.store(pkt.nonce, Ordering::Relaxed);
            store.clear();
            persist::clear(&data_path);
        }
        // At this point the nonce matches (or was just accepted).

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
