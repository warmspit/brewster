// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

//! Axum HTTP handlers — serves the same API the dashboard already expects.

use std::path::PathBuf;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::BroadcastStream;
use tower::ServiceBuilder;
use tower_http::cors::CorsLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tracing::warn;

use crate::store::Store;

#[derive(Clone)]
struct AppState {
    store: Store,
    device_name: String,
    /// Port the device's embedded HTTP server listens on (default 80).
    device_http_port: u16,
    /// Shared reqwest client for forwarding commands to the device.
    client: reqwest::Client,
    /// Fires once per received UDP packet; SSE clients subscribe to this.
    notify: broadcast::Sender<()>,
    /// Path to the JSONL persistence file; needed to clear it on history/clear.
    data_file: std::path::PathBuf,
}

pub fn router(
    store: Store,
    device_name: String,
    device_http_port: u16,
    web_dir: PathBuf,
    notify: broadcast::Sender<()>,
    data_file: std::path::PathBuf,
) -> Router {
    // Static file service for the dashboard assets.
    // SetResponseHeaderLayer::if_not_present adds Cache-Control: no-cache to static files
    // so browsers always revalidate JS/HTML after a server update. API handlers that
    // already set Cache-Control: no-store are unaffected (header already present).
    let serve = ServiceBuilder::new()
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache"),
        ))
        .service(tower_http::services::ServeDir::new(&web_dir).fallback(
            tower_http::services::ServeFile::new(web_dir.join("index.html")),
        ));

    Router::new()
        .route("/status", get(get_status))
        .route("/stats", get(get_stats))
        .route("/history", get(get_history))
        .route("/history/clear", post(post_clear))
        .route("/temperature", post(post_temperature))
        // Dashboard start/stop: updates server collecting state and forwards to device.
        .route("/collection/start", post(collecting_start))
        .route("/collection/stop", post(collecting_stop))
        .route("/events", get(get_events))
        .layer(CorsLayer::permissive())
        .with_state(AppState {
            store,
            device_name,
            device_http_port,
            client: reqwest::Client::new(),
            notify,
            data_file,
        })
        .fallback_service(serve)
}

// ── /status ──────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct SensorJson {
    index: u8,
    name: String,
    temperature_c: Option<f32>,
    temperature_f: Option<f32>,
    error: &'static str,
}

#[derive(Serialize)]
struct PidJson {
    target_c: f32,
    target_f: f32,
    output_percent: f32,
    window_step: u8,
    on_steps: u8,
    relay_on: bool,
    heat_on: bool,
    deadband_c: f32,
    /// Active PID term contributions (%).
    pid_p_pct: f32,
    pid_i_pct: f32,
    pid_d_pct: f32,
}

#[derive(Serialize)]
struct NtpJson {
    synced: bool,
}

#[derive(Serialize)]
struct SystemJson {
    ip: String,
    device_http_port: u16,
    collecting: bool,
    uptime_s: u32,
    seq: u32,
    packets_dropped: u64,
    ntp: NtpJson,
}

#[derive(Serialize)]
struct StatusJson {
    device: String,
    hostname: String,
    sensors: Vec<SensorJson>,
    control_probe_index: u8,
    pid: PidJson,
    system: SystemJson,
}

async fn get_status(
    State(AppState {
        store,
        device_name,
        device_http_port,
        ..
    }): State<AppState>,
) -> Response {
    let (latest, collecting) = store.latest_with_collecting();
    let Some(pkt) = latest else {
        return (StatusCode::SERVICE_UNAVAILABLE, "no data yet").into_response();
    };
    let stats = store.telemetry_stats();

    let sensor_count = pkt.sensor_count as usize;
    let sensors: Vec<SensorJson> = (0..sensor_count.min(3))
        .map(|i| {
            let temp_c = if pkt.temps[i].is_nan() {
                None
            } else {
                Some(pkt.temps[i])
            };
            SensorJson {
                index: i as u8,
                name: format!("probe-{}", i + 1),
                temperature_f: temp_c.map(|c| c * 9.0 / 5.0 + 32.0),
                temperature_c: temp_c,
                error: sensor_error_label(pkt.sensor_status[i]),
            }
        })
        .collect();

    let target_c = pkt.target_c;
    let ip = fmt_ip(pkt.device_ip);
    let hostname = std::str::from_utf8(&pkt.hostname)
        .unwrap_or("")
        .trim_end_matches('\0')
        .to_string();

    let body = StatusJson {
        device: device_name,
        hostname,
        sensors,
        control_probe_index: 0,
        pid: PidJson {
            target_c,
            target_f: target_c * 9.0 / 5.0 + 32.0,
            output_percent: pkt.output_pct as f32,
            window_step: pkt.window_step,
            on_steps: pkt.on_steps,
            relay_on: pkt.relay_on,
            heat_on: pkt.heat_on,
            deadband_c: pkt.deadband_c,
            pid_p_pct: pkt.pid_p_pct as f32,
            pid_i_pct: pkt.pid_i_pct as f32,
            pid_d_pct: pkt.pid_d_pct as f32,
        },
        system: SystemJson {
            ip,
            device_http_port,
            collecting,
            uptime_s: pkt.uptime_s,
            seq: pkt.seq,
            packets_dropped: stats.packets_dropped,
            ntp: NtpJson {
                synced: pkt.ntp_synced,
            },
        },
    };

    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (headers, Json(body)).into_response()
}

// ── /history ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct HistoryQuery {
    points: Option<usize>,
}

#[derive(Serialize)]
struct HistoryJson {
    sample_interval_s: u32,
    total_samples: u32,
    // Each point: [seq, temp_c, target_c, output_pct, window_step, on_steps, relay_on,
    //              extra1, extra2, pid_p_pct, pid_i_pct, pid_d_pct, t_s, gap_before]
    points: Vec<serde_json::Value>,
}

async fn get_history(
    State(AppState { store, .. }): State<AppState>,
    Query(q): Query<HistoryQuery>,
) -> Response {
    let max = q.points.unwrap_or(2000).clamp(1, 10000);
    let (pts, total_samples, sample_interval_s) = store.history_data(max);

    let points: Vec<serde_json::Value> = pts
        .iter()
        .map(|p| {
            let relay: i32 = if p.relay_on { 1 } else { 0 };
            let e1 = match p.extra_temps[0] {
                Some(v) => serde_json::Value::from((v * 100.0).round() / 100.0),
                None => serde_json::Value::Null,
            };
            let e2 = match p.extra_temps[1] {
                Some(v) => serde_json::Value::from((v * 100.0).round() / 100.0),
                None => serde_json::Value::Null,
            };
            let temp = match p.temp_c {
                Some(v) => serde_json::Value::from((v * 100.0).round() / 100.0),
                None => serde_json::Value::Null,
            };
            let gap: i32 = if p.gap_before { 1 } else { 0 };
            serde_json::json!([
                p.seq,
                temp,
                (p.target_c * 100.0).round() / 100.0,
                p.output_pct,
                p.window_step,
                p.on_steps,
                relay,
                e1,
                e2,
                p.pid_p_pct,
                p.pid_i_pct,
                p.pid_d_pct,
                p.t_s, // col 12: wall-clock unix seconds
                gap,   // col 13: 1 if real data gap precedes this point
            ])
        })
        .collect();

    let body = HistoryJson {
        sample_interval_s,
        total_samples,
        points,
    };

    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (headers, Json(body)).into_response()
}

// ── /history/clear ────────────────────────────────────────────────────────────

async fn post_clear(
    State(AppState {
        store,
        client,
        device_http_port,
        data_file,
        ..
    }): State<AppState>,
) -> impl IntoResponse {
    // Capture device IP *before* clearing (clear() sets latest = None).
    let device_ip = store.latest().map(|pkt| pkt.device_ip);
    store.clear();
    // Clear the on-disk JSONL file immediately so a server restart does not
    // resurrect the cleared data.
    crate::persist::clear(&data_file);
    // Forward to device so its ring-buffer is also cleared.
    if let Some(ip) = device_ip {
        let url = format!("http://{}:{}/history/clear", fmt_ip(ip), device_http_port);
        match client
            .post(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {}
            Ok(resp) => warn!(
                "history/clear forward to {url}: device replied {}",
                resp.status()
            ),
            Err(e) => warn!("history/clear forward to {url}: {e}"),
        }
    } else {
        warn!("history/clear forward: no telemetry yet — device IP unknown");
    }
    Json(serde_json::json!({"ok": true}))
}

// ── /temperature ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct TemperatureBody {
    temperature_c: f32,
}

async fn post_temperature(
    State(AppState {
        store,
        client,
        device_http_port,
        ..
    }): State<AppState>,
    Json(body): Json<TemperatureBody>,
) -> Response {
    let device_ip = match store.latest() {
        Some(pkt) => pkt.device_ip,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "no telemetry received yet — device IP unknown"})),
            )
                .into_response();
        }
    };

    let url = format!(
        "http://{}:{}/temperature",
        fmt_ip(device_ip),
        device_http_port
    );

    match client
        .post(&url)
        .json(&serde_json::json!({"temperature_c": body.temperature_c}))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => Json(serde_json::json!({
            "ok": true,
            "temperature_c": body.temperature_c,
        }))
        .into_response(),
        Ok(resp) => {
            let status = resp.status();
            warn!("temperature forward to {url}: device replied {status}");
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": format!("device replied {status}")})),
            )
                .into_response()
        }
        Err(e) => {
            warn!("temperature forward to {url}: {e}");
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": format!("could not reach device: {e}")})),
            )
                .into_response()
        }
    }
}

async fn collecting_start(
    State(AppState {
        store,
        client,
        device_http_port,
        ..
    }): State<AppState>,
) -> impl IntoResponse {
    store.set_collecting(true);
    forward_collection(&store, &client, device_http_port, true).await;
    Json(serde_json::json!({"ok": true, "collecting": true}))
}

async fn collecting_stop(
    State(AppState {
        store,
        client,
        device_http_port,
        ..
    }): State<AppState>,
) -> impl IntoResponse {
    store.set_collecting(false);
    forward_collection(&store, &client, device_http_port, false).await;
    Json(serde_json::json!({"ok": true, "collecting": false}))
}

/// Forward a collection start/stop command to the device's embedded HTTP server.
/// Logs but does not fail — the server state has already been updated.
async fn forward_collection(store: &Store, client: &reqwest::Client, port: u16, enabled: bool) {
    let ip = match store.latest() {
        Some(pkt) => pkt.device_ip,
        None => {
            warn!("collection forward: no telemetry yet — device IP unknown");
            return;
        }
    };
    let path = if enabled { "start" } else { "stop" };
    let url = format!("http://{}:{}/collection/{}", fmt_ip(ip), port, path);
    match client
        .post(&url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            // no log needed — the dashboard poll will confirm
        }
        Ok(resp) => warn!(
            "collection forward to {url}: device replied {}",
            resp.status()
        ),
        Err(e) => warn!("collection forward to {url}: {e}"),
    }
}

// ── /stats ────────────────────────────────────────────────────────────────────────

async fn get_stats(State(AppState { store, .. }): State<AppState>) -> impl IntoResponse {
    let stats = store.telemetry_stats();
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (headers, Json(stats)).into_response()
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn sensor_error_label(code: u8) -> &'static str {
    // Codes match the firmware's SensorStatus enum in status.rs.
    match code {
        0 => "none",
        1 => "bus_stuck_low",
        2 => "no_device",
        3 => "crc_mismatch",
        _ => "unknown",
    }
}

fn fmt_ip(ip: [u8; 4]) -> String {
    format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])
}

// ── /events (SSE) ─────────────────────────────────────────────────────────────

/// Server-Sent Events stream — fires a `pkt` event whenever a UDP telemetry
/// packet is received from the device.  Browsers use this to update
/// immediately rather than waiting for the 5-second poll interval.
async fn get_events(State(AppState { notify, .. }): State<AppState>) -> impl IntoResponse {
    let rx = notify.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|r| {
        r.ok()
            .map(|()| Ok::<Event, std::convert::Infallible>(Event::default().event("pkt").data("")))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}
