// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

//! Brewster LAN server — receives UDP telemetry from the device and serves
//! the dashboard HTTP API to any browser on the local network.
//!
//! # Configuration (environment variables)
//!
//! | Variable           | Default                  | Description                             |
//! |--------------------|---------------------------|-----------------------------------------|
//! | `UDP_PORT`         | `47890`                  | Port the server listens on for packets  |
//! | `HTTP_PORT`        | `8080`                   | Port the HTTP server binds to           |
//! | `DEVICE_HTTP_PORT` | `80`                     | Port the device's embedded HTTP server  |
//! | `DEVICE_NAME`      | `brewster`               | Display name and mDNS instance label    |
//! | `RETENTION_HOURS`  | `72`                     | How many hours of data to keep          |
//! | `WEB_DIR`          | `../web`                 | Path to the dashboard static assets     |
//! | `DATA_FILE`        | `./brewster-data.json`   | Path to the persistence file            |
//! | `SENSOR_NAMES`     | *(empty)*                | Comma-separated probe names e.g. `Freezer,Thermal Well,Ambient` |
//!
//! # Usage
//!
//! ```sh
//! cd server && cargo run --release
//! # or with options:
//! UDP_PORT=47890 HTTP_PORT=8080 RETENTION_HOURS=168 WEB_DIR=../web cargo run --release
//! ```

mod discovery;
mod http;
mod packet;
mod persist;
mod store;
mod udp;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use tokio::net::UdpSocket;
use tracing::info;

use store::Store;

#[tokio::main]
async fn main() {
    // Initialise logging. RUST_LOG controls the filter (default: info).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let udp_port: u16 = env_u16("UDP_PORT", 47890);
    let http_port: u16 = env_u16("HTTP_PORT", 8080);
    let device_http_port: u16 = env_u16("DEVICE_HTTP_PORT", 80);
    let retention_hours: u64 = env_u64("RETENTION_HOURS", 1440);
    let web_dir = PathBuf::from(std::env::var("WEB_DIR").unwrap_or_else(|_| "../web".into()));
    let device_name = std::env::var("DEVICE_NAME").unwrap_or_else(|_| "brewster".into());
    let sensor_names: Vec<String> = std::env::var("SENSOR_NAMES")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let data_file =
        PathBuf::from(std::env::var("DATA_FILE").unwrap_or_else(|_| "./brewster-data.json".into()));

    info!(
        "{}-server starting  udp=:{udp_port}  http=:{http_port}  \
         retention={}h  web={web_dir:?}  data={data_file:?}",
        device_name, retention_hours
    );

    let store = Store::new(Duration::from_secs(retention_hours * 3600));
    persist::load(&store, &data_file);

    // Notify channel: fires once per received UDP packet so SSE clients update immediately.
    let (pkt_tx, _) = tokio::sync::broadcast::channel::<()>(16);

    // ── UDP listener ──────────────────────────────────────────────────────────
    let udp_addr: SocketAddr = format!("0.0.0.0:{udp_port}").parse().unwrap();
    let sock = UdpSocket::bind(udp_addr).await.expect("bind UDP");
    info!("UDP listening on {udp_addr}");

    tokio::spawn(udp::run(
        sock,
        store.clone(),
        pkt_tx.clone(),
        data_file.clone(),
    ));

    // ── Discovery (UDP broadcast + mDNS) ─────────────────────────────────────
    info!(
        "discovery: DISCOVERY_PORT={}  service=_brewster._udp.local.  instance={}-server",
        discovery::DISCOVERY_PORT,
        device_name,
    );
    tokio::spawn(discovery::run(udp_port, http_port, device_name.clone()));

    // ── HTTP server ───────────────────────────────────────────────────────────
    let app = http::router(
        store,
        device_name,
        device_http_port,
        web_dir,
        pkt_tx,
        data_file,
        sensor_names,
    );
    let http_addr: SocketAddr = format!("0.0.0.0:{http_port}").parse().unwrap();
    let listener = tokio::net::TcpListener::bind(http_addr).await.unwrap();
    info!("HTTP listening on http://{http_addr}");

    axum::serve(listener, app).await.unwrap();
}

fn env_u16(key: &str, default: u16) -> u16 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
