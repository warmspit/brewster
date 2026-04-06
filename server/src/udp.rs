// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

//! UDP telemetry receiver — decodes incoming packets and inserts into the store.

use std::path::PathBuf;

use tokio::net::UdpSocket;
use tokio::sync::broadcast;
use tracing::{error, warn};

use crate::packet;
use crate::persist;
use crate::store::Store;

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
        match packet::Packet::decode(&buf[..len]) {
            Some(pkt) => {
                if pkt.history_clear {
                    store.clear();
                    persist::clear(&data_path);
                }
                if let Some(record) = store.insert(pkt) {
                    persist::append(&record, &data_path);
                }
                let _ = notify.send(());
            }
            None => {
                warn!("udp: bad packet from {peer} ({len} bytes)");
            }
        }
    }
}
