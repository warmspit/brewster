// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

use alloc::string::ToString;
use core::fmt::Write as _;
use embassy_net::Stack;
use embassy_net::tcp::{Error as TcpError, TcpSocket};
use embassy_time::{Duration, Timer};
use esp_println::println;

use crate::firmware::{metrics, status};

const DASHBOARD_HTML_TEMPLATE: &str = r#"<!doctype html>
<html lang="en">
    <head>
        <meta charset="utf-8" />
        <meta name="viewport" content="width=device-width, initial-scale=1" />
        <title>Brewster Dashboard</title>
        <style>
            :root {
                --bg-1: #09101b;
                --bg-2: #13253d;
                --panel: rgba(9, 20, 32, 0.74);
                --panel-border: rgba(130, 184, 235, 0.24);
                --text: #e6f1ff;
                --muted: #9fb4cb;
                --accent: #40c4ff;
                --ok: #40d990;
                --warn: #ffb74d;
                --danger: #ff6e6e;
                --card-shadow: 0 10px 40px rgba(0, 0, 0, 0.35);
            }

            * {
                box-sizing: border-box;
            }

            body {
                margin: 0;
                min-height: 100vh;
                font-family: "Avenir Next", "Trebuchet MS", "Segoe UI", sans-serif;
                color: var(--text);
                background:
                    radial-gradient(circle at 20% 10%, rgba(64, 196, 255, 0.22), transparent 45%),
                    radial-gradient(circle at 85% 80%, rgba(64, 217, 144, 0.16), transparent 52%),
                    linear-gradient(145deg, var(--bg-1), var(--bg-2));
                padding: 24px;
            }

            .dashboard {
                max-width: 1180px;
                margin: 0 auto;
                display: grid;
                gap: 16px;
            }

            .headline {
                display: flex;
                justify-content: space-between;
                align-items: baseline;
                gap: 12px;
            }

            .headline h1 {
                margin: 0;
                font-size: clamp(1.35rem, 3vw, 2.1rem);
                letter-spacing: 0.04em;
                text-transform: uppercase;
            }

            .headline .meta {
                color: var(--muted);
                font-size: 0.95rem;
            }

            .menu-wrap {
                position: relative;
                margin-left: auto;
                align-self: center;
            }

            .menu-btn {
                background: #0b1624;
                border: 1px solid rgba(130, 184, 235, 0.3);
                border-radius: 8px;
                color: var(--muted);
                cursor: pointer;
                font-size: 1.3rem;
                line-height: 1;
                padding: 5px 11px;
            }

            .menu-btn:hover {
                color: var(--text);
                border-color: rgba(130, 184, 235, 0.6);
            }

            .menu-dropdown {
                display: none;
                position: absolute;
                right: 0;
                top: calc(100% + 8px);
                background: #0b1624;
                border: 1px solid var(--panel-border);
                border-radius: 12px;
                box-shadow: var(--card-shadow);
                min-width: 200px;
                padding: 6px 0;
                z-index: 100;
            }

            .menu-dropdown.open {
                display: block;
            }

            .menu-item {
                background: none;
                border: none;
                color: var(--text);
                cursor: pointer;
                display: block;
                font-size: 0.95rem;
                padding: 10px 18px;
                text-align: left;
                width: 100%;
            }

            .menu-item:hover {
                background: rgba(130, 184, 235, 0.08);
            }

            .menu-item.danger {
                color: #ff6e6e;
            }

            .menu-item.success {
                color: #40d990;
            }

            .menu-item:disabled {
                opacity: 0.35;
                cursor: default;
            }

            .menu-section {
                padding: 10px 12px;
                border-bottom: 1px solid rgba(130, 184, 235, 0.16);
            }

            .menu-section .kpi-sub {
                margin-top: 0;
                margin-bottom: 8px;
                font-size: 0.78rem;
                letter-spacing: 0.06em;
                text-transform: uppercase;
            }

            .menu-section .target-control {
                margin-top: 0;
            }

            .grid {
                display: grid;
                grid-template-columns: repeat(12, minmax(0, 1fr));
                gap: 14px;
            }

            .card {
                background: var(--panel);
                border: 1px solid var(--panel-border);
                border-radius: 16px;
                box-shadow: var(--card-shadow);
                backdrop-filter: blur(8px);
                padding: 16px;
                min-width: 0;
            }

            .top-row {
                display: flex;
                gap: 14px;
            }
            .top-row > .card {
                flex: 1;
                min-width: 0;
            }
            .span-3 { grid-column: span 3; }
            .span-4 { grid-column: span 4; }
            .span-6 { grid-column: span 6; }
            .span-8 { grid-column: span 8; }
            .span-12 { grid-column: span 12; }

            .kpi-title {
                color: var(--muted);
                font-size: 0.85rem;
                text-transform: uppercase;
                letter-spacing: 0.08em;
            }

            .chart-title {
                position: relative;
                text-align: left;
            }

            .chart-title-center {
                position: absolute;
                left: 50%;
                transform: translateX(-50%);
                white-space: nowrap;
            }

            .kpi-value {
                margin-top: 6px;
                font-size: clamp(1.5rem, 3vw, 2.4rem);
                font-weight: 700;
                line-height: 1.1;
            }

            #ip {
                font-size: clamp(0.8rem, 1.4vw, 1.05rem);
                overflow-wrap: anywhere;
                word-break: keep-all;
                white-space: nowrap;
            }

            .kpi-sub {
                margin-top: 8px;
                color: var(--muted);
                font-size: 0.9rem;
            }

            .target-control {
                margin-top: 10px;
                display: grid;
                gap: 8px;
            }

            .target-input-row {
                display: flex;
                gap: 8px;
                align-items: center;
            }

            .target-input {
                width: 100%;
                min-width: 0;
                background: rgba(6, 12, 20, 0.8);
                border: 1px solid rgba(130, 184, 235, 0.22);
                border-radius: 10px;
                color: var(--text);
                padding: 8px 10px;
                font-size: 0.95rem;
            }

            .target-button {
                background: linear-gradient(135deg, #40c4ff, #2fa8e0);
                border: 0;
                color: #04121f;
                border-radius: 10px;
                padding: 8px 12px;
                font-weight: 700;
                cursor: pointer;
                white-space: nowrap;
            }

            .target-button:disabled {
                opacity: 0.6;
                cursor: default;
            }

            #target-feedback {
                min-height: 1.1em;
            }

            .status-pill {
                display: inline-block;
                padding: 4px 10px;
                border-radius: 999px;
                font-size: 0.8rem;
                letter-spacing: 0.06em;
                text-transform: uppercase;
            }

            .status-ok { background: rgba(64, 217, 144, 0.18); color: var(--ok); }
            .status-warn { background: rgba(255, 183, 77, 0.18); color: var(--warn); }
            .status-danger { background: rgba(255, 110, 110, 0.16); color: var(--danger); }

            .chart {
                width: 100%;
                height: 220px;
                border-radius: 10px;
                background: rgba(8, 15, 25, 0.72);
                border: 1px solid rgba(130, 184, 235, 0.14);
            }

            .legend {
                display: flex;
                flex-wrap: wrap;
                gap: 8px 12px;
                margin-top: 10px;
                color: var(--muted);
                font-size: 0.82rem;
            }

            .legend-item {
                display: inline-flex;
                align-items: center;
                gap: 6px;
            }

            .legend-dot {
                width: 10px;
                height: 10px;
                border-radius: 999px;
                display: inline-block;
            }

            .rows {
                display: grid;
                gap: 10px;
            }

            .row {
                display: flex;
                justify-content: space-between;
                gap: 12px;
                color: var(--muted);
                font-size: 0.92rem;
            }

            .row strong {
                color: var(--text);
                font-weight: 600;
            }

            @media (max-width: 980px) {
                .top-row {
                    flex-direction: column;
                }
                .span-4,
                .span-6,
                .span-8 {
                    grid-column: span 12;
                }
            }
        </style>
    </head>
    <body>
        <main class="dashboard">
            <header class="headline">
                <h1 id="title">__HOSTNAME__ CONTROL PANEL</h1>
                <div class="meta" id="updated">Waiting for data...</div>
                <div class="menu-wrap">
                    <button class="menu-btn" id="menu-btn" aria-label="Menu" aria-expanded="false">&#9776;</button>
                    <div class="menu-dropdown" id="menu-dropdown" role="menu">
                        <div class="menu-section">
                            <div class="kpi-sub">Set Target Temperature</div>
                            <div class="target-control">
                                <div class="target-input-row">
                                    <input id="target-input" class="target-input" type="number" min="-20" max="25" step="0.1" placeholder="Set target C" />
                                    <button id="target-submit" class="target-button" type="button">Apply</button>
                                </div>
                                <div class="kpi-sub" id="target-feedback"></div>
                            </div>
                        </div>
                        <div class="menu-section">
                            <div class="kpi-sub">Data Collection</div>
                            <button class="menu-item success" id="start-data" role="menuitem">Start collection</button>
                            <button class="menu-item" id="stop-data" role="menuitem">Stop collection</button>
                        </div>
                        <button class="menu-item danger" id="clear-data" role="menuitem">Clear all saved data</button>
                    </div>
                </div>
            </header>

            <section class="grid">
                <div class="top-row span-12">
                <article class="card">
                    <div class="kpi-title">Temperature</div>
                    <div class="kpi-value" id="temp">--.- C</div>
                    <div class="kpi-sub" id="temp-secondary">--.- F</div>
                </article>

                <article class="card">
                    <div class="kpi-title">Target</div>
                    <div class="kpi-value" id="target">--.- C</div>
                    <div class="kpi-sub" id="target-secondary">--.- F</div>
                </article>

                <article class="card">
                    <div class="kpi-title">PID Output</div>
                    <div class="kpi-value" id="pid">--.-%</div>
                    <div class="kpi-sub" id="relay">Relay --</div>
                </article>

                <article class="card">
                    <div class="kpi-title">Network</div>
                    <div class="kpi-value" id="ip">--</div>
                    <div class="kpi-sub"><span id="ntp-pill" class="status-pill status-warn">NTP pending</span></div>
                </article>
                </div>

                <article class="card span-12">
                    <div class="kpi-title chart-title">Temperature Trend (live)<span class="chart-title-center" id="temp-chart-probe">--</span></div>
                    <canvas id="temp-chart" class="chart" width="1120" height="220"></canvas>
                </article>

                <article class="card span-12">
                    <div class="kpi-title chart-title">PID Trend (all parameters)<span class="chart-title-center" id="pid-chart-probe">--</span></div>
                    <canvas id="pid-chart" class="chart" width="1120" height="220"></canvas>
                    <div class="legend">
                        <span class="legend-item"><span class="legend-dot" style="background:#f7d774"></span>target_c</span>
                        <span class="legend-item"><span class="legend-dot" style="background:#6ec5ff"></span>kp</span>
                        <span class="legend-item"><span class="legend-dot" style="background:#8ef0c8"></span>ki</span>
                        <span class="legend-item"><span class="legend-dot" style="background:#b28cff"></span>kd</span>
                        <span class="legend-item"><span class="legend-dot" style="background:#ff8d6e"></span>output_pct</span>
                        <span class="legend-item"><span class="legend-dot" style="background:#7cf3ff"></span>window_step</span>
                        <span class="legend-item"><span class="legend-dot" style="background:#ffb3d1"></span>on_steps</span>
                        <span class="legend-item"><span class="legend-dot" style="background:#ffffff"></span>relay_on</span>
                    </div>
                </article>

            </section>
        </main>

        <script type="module" src="/dashboard.js"></script>
    </body>
</html>
"#;

const DASHBOARD_JS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/web/dashboard.js"));

fn dashboard_html() -> alloc::string::String {
    let hostname = crate::device_hostname().to_ascii_uppercase();
    DASHBOARD_HTML_TEMPLATE.replace("__HOSTNAME__", &hostname)
}

enum ResponseBody {
    Static(&'static str),
    Owned(alloc::string::String),
}

impl ResponseBody {
    fn as_str(&self) -> &str {
        match self {
            Self::Static(body) => body,
            Self::Owned(body) => body,
        }
    }
}

fn temperature_ok_json(temp_c: f32) -> alloc::string::String {
    let mut body = alloc::string::String::with_capacity(80);
    let _ = write!(
        body,
        concat!(
            "{{\n",
            "  \"ok\": true,\n",
            "  \"temperature_c\": {:.1},\n",
            "  \"temperature_f\": {:.1}\n",
            "}}\n"
        ),
        temp_c,
        temp_c * 9.0 / 5.0 + 32.0
    );
    body
}

fn probe_name_ok_json(name: &str) -> alloc::string::String {
    let mut body = alloc::string::String::with_capacity(96);
    let _ = write!(
        body,
        concat!(
            "{{\n",
            "  \"ok\": true,\n",
            "  \"probe_name\": \"{}\"\n",
            "}}\n"
        ),
        name
    );
    body
}

fn collection_ok_json(enabled: bool) -> alloc::string::String {
    let mut body = alloc::string::String::with_capacity(64);
    let _ = write!(
        body,
        concat!(
            "{{\n",
            "  \"ok\": true,\n",
            "  \"collecting\": {}\n",
            "}}\n"
        ),
        if enabled { "true" } else { "false" }
    );
    body
}

fn parse_json_string_field(body: &str, key: &str) -> Option<alloc::string::String> {
    let mut pattern = alloc::string::String::with_capacity(key.len() + 2);
    pattern.push('"');
    pattern.push_str(key);
    pattern.push('"');
    let key_pos = body.find(&pattern)?;
    let after_key = &body[key_pos + pattern.len()..];
    let colon_pos = after_key.find(':')?;
    let mut value = after_key[colon_pos + 1..].trim_start();
    if !value.starts_with('"') {
        return None;
    }
    value = &value[1..];
    let end_quote = value.find('"')?;
    Some(value[..end_quote].to_string())
}

fn parse_probe_name(buf: &[u8]) -> Option<alloc::string::String> {
    let header_end = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let body = core::str::from_utf8(&buf[header_end + 4..]).ok()?.trim();
    if body.is_empty() {
        return None;
    }
    if body.starts_with('{') {
        return parse_json_string_field(body, "probe_name")
            .or_else(|| parse_json_string_field(body, "name"));
    }
    if body.starts_with('"') && body.ends_with('"') && body.len() >= 2 {
        return Some(body[1..body.len() - 1].to_string());
    }
    Some(body.to_string())
}

#[derive(Clone)]
enum ParsedRequest {
    GetDashboard,
    GetDashboardScript,
    GetStatus,
    GetHistory(usize),
    GetMetrics,
    PostTemperature(f32),
    PostCollection(bool),
    PostHistoryClear,
    PostProbeName(alloc::string::String),
    BadRequest,
    NotFound,
}

fn parse_history_points(buf: &[u8]) -> usize {
    const DEFAULT_POINTS: usize = 400;
    const MAX_POINTS: usize = 1000;

    let line_end = match buf.windows(2).position(|w| w == b"\r\n") {
        Some(i) => i,
        None => return DEFAULT_POINTS,
    };
    let line = match core::str::from_utf8(&buf[..line_end]) {
        Ok(v) => v,
        Err(_) => return DEFAULT_POINTS,
    };
    let query_pos = match line.find("?") {
        Some(i) => i,
        None => return DEFAULT_POINTS,
    };
    let query = &line[query_pos + 1..];
    for pair in query.split('&') {
        if let Some(value) = pair.strip_prefix("points=")
            && let Ok(parsed) = value.parse::<usize>()
        {
            return parsed.clamp(1, MAX_POINTS);
        }
    }
    DEFAULT_POINTS
}

fn parse_request(buf: &[u8]) -> ParsedRequest {
    if buf.starts_with(b"GET /dashboard.js ")
        || buf.starts_with(b"GET /dashboard.js?")
        || buf.starts_with(b"GET /dashboard.js\r")
    {
        return ParsedRequest::GetDashboardScript;
    }

    if buf.starts_with(b"GET /panel ")
        || buf.starts_with(b"GET /panel?")
        || buf.starts_with(b"GET /panel\r")
    {
        return ParsedRequest::GetDashboard;
    }

    if buf.starts_with(b"GET / ") || buf.starts_with(b"GET /?") || buf.starts_with(b"GET /\r") {
        return ParsedRequest::GetDashboard;
    }

    if buf.starts_with(b"GET /status ")
        || buf.starts_with(b"GET /status?")
        || buf.starts_with(b"GET /status\r")
    {
        return ParsedRequest::GetStatus;
    }

    if buf.starts_with(b"GET /history ")
        || buf.starts_with(b"GET /history?")
        || buf.starts_with(b"GET /history\r")
    {
        return ParsedRequest::GetHistory(parse_history_points(buf));
    }

    if buf.starts_with(b"GET /metrics ")
        || buf.starts_with(b"GET /metrics?")
        || buf.starts_with(b"GET /metrics\r")
    {
        return ParsedRequest::GetMetrics;
    }

    if buf.starts_with(b"POST /temperature ") || buf.starts_with(b"POST /temperature\r") {
        let header_end = match buf.windows(4).position(|w| w == b"\r\n\r\n") {
            Some(i) => i,
            None => return ParsedRequest::BadRequest,
        };
        let body = &buf[header_end + 4..];
        let key_pos = match body.windows(13).position(|w| w == b"temperature_c") {
            Some(i) => i,
            None => return ParsedRequest::BadRequest,
        };
        let after_key = &body[key_pos + 13..];
        let colon_pos = match after_key.iter().position(|&b| b == b':') {
            Some(i) => i,
            None => return ParsedRequest::BadRequest,
        };
        let after_colon = &after_key[colon_pos + 1..];
        let value_start = match after_colon.iter().position(|&b| !matches!(b, b' ' | b'\t')) {
            Some(i) => i,
            None => return ParsedRequest::BadRequest,
        };
        let value_bytes = &after_colon[value_start..];
        let value_end = value_bytes
            .iter()
            .position(|&b| matches!(b, b',' | b'}' | b' ' | b'\t' | b'\r' | b'\n'))
            .unwrap_or(value_bytes.len());
        if let Ok(s) = core::str::from_utf8(&value_bytes[..value_end])
            && let Ok(v) = s.parse::<f32>()
        {
            return ParsedRequest::PostTemperature(v);
        }
        return ParsedRequest::BadRequest;
    }

    if buf.starts_with(b"POST /probe-name ") || buf.starts_with(b"POST /probe-name\r") {
        return match parse_probe_name(buf) {
            Some(name) => ParsedRequest::PostProbeName(name),
            None => ParsedRequest::BadRequest,
        };
    }

    if buf.starts_with(b"POST /collection/start ") || buf.starts_with(b"POST /collection/start\r") {
        return ParsedRequest::PostCollection(true);
    }

    if buf.starts_with(b"POST /collection/stop ") || buf.starts_with(b"POST /collection/stop\r") {
        return ParsedRequest::PostCollection(false);
    }

    if buf.starts_with(b"POST /history/clear ") || buf.starts_with(b"POST /history/clear\r") {
        return ParsedRequest::PostHistoryClear;
    }

    ParsedRequest::NotFound
}

async fn socket_write_all(socket: &mut TcpSocket<'_>, mut data: &[u8]) -> Result<(), TcpError> {
    while !data.is_empty() {
        let written = socket.write(data).await?;
        if written == 0 {
            return Err(TcpError::ConnectionReset);
        }
        data = &data[written..];
    }

    Ok(())
}

async fn socket_write_http_response(
    socket: &mut TcpSocket<'_>,
    status_line: &str,
    content_type: &str,
    cache_control: &str,
    body: &str,
) -> Result<(), TcpError> {
    let mut header = alloc::string::String::with_capacity(128);
    let _ = write!(
        header,
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nCache-Control: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status_line,
        content_type,
        cache_control,
        body.len(),
    );
    socket_write_all(socket, header.as_bytes()).await?;
    socket_write_all(socket, body.as_bytes()).await
}

#[allow(
    clippy::large_stack_frames,
    reason = "the async HTTP task state machine holds socket state across awaits; buffers are kept in static storage"
)]
#[embassy_executor::task]
pub(super) async fn http_status_task(stack: Stack<'static>) {
    let rx_buffer = super::HTTP_RX_BUFFER.take();
    let tx_buffer = super::HTTP_TX_BUFFER.take();

    println!("http: status endpoint enabled on port {}", super::HTTP_PORT);

    loop {
        stack.wait_config_up().await;

        let mut socket = TcpSocket::new(stack, rx_buffer, tx_buffer);
        socket.set_timeout(Some(Duration::from_secs(10)));

        match socket.accept(super::HTTP_PORT).await {
            Ok(()) => {}
            Err(error) => {
                println!("http: accept failed: {:?}", error);
                Timer::after(Duration::from_millis(250)).await;
                continue;
            }
        }

        status::http_exchange_begin();

        let remote = socket.remote_endpoint();
        let parsed = match socket
            .read_with(|buf| {
                if buf.is_empty() {
                    (0, None)
                } else {
                    (buf.len(), Some(parse_request(buf)))
                }
            })
            .await
        {
            Ok(Some(req)) => req,
            Ok(None) => {
                println!("http: empty request from {:?}", remote);
                status::http_exchange_mark_error();
                socket.abort();
                let _ = socket.flush().await;
                status::http_exchange_end();
                continue;
            }
            Err(error) => {
                println!("http: read failed from {:?}: {:?}", remote, error);
                status::http_exchange_mark_error();
                socket.abort();
                let _ = socket.flush().await;
                status::http_exchange_end();
                continue;
            }
        };

        let (status_line, content_type, cache_control, body) = match parsed {
            ParsedRequest::GetDashboard => {
                println!("http: serving dashboard to {:?}", remote);
                (
                    "200 OK",
                    "text/html; charset=utf-8",
                    "no-store",
                    ResponseBody::Owned(dashboard_html()),
                )
            }
            ParsedRequest::GetDashboardScript => {
                println!("http: serving dashboard script to {:?}", remote);
                (
                    "200 OK",
                    "application/javascript; charset=utf-8",
                    "no-store",
                    ResponseBody::Static(DASHBOARD_JS),
                )
            }
            ParsedRequest::GetStatus => {
                println!("http: serving status to {:?}", remote);
                (
                    "200 OK",
                    "application/json",
                    "no-store",
                    ResponseBody::Owned(metrics::json()),
                )
            }
            ParsedRequest::GetHistory(points) => {
                println!("http: serving history({}) to {:?}", points, remote);
                (
                    "200 OK",
                    "application/json",
                    "no-store",
                    ResponseBody::Owned(metrics::history_json(points)),
                )
            }
            ParsedRequest::GetMetrics => {
                println!("http: serving metrics to {:?}", remote);
                (
                    "200 OK",
                    "text/plain; version=0.0.4; charset=utf-8",
                    "no-store",
                    ResponseBody::Owned(metrics::prometheus()),
                )
            }
            ParsedRequest::PostTemperature(temp) => {
                if !(-20.0_f32..=25.0).contains(&temp) {
                    (
                        "400 Bad Request",
                        "application/json",
                        "no-store",
                        ResponseBody::Static("{\n  \"error\": \"out_of_range\"\n}\n"),
                    )
                } else {
                    match status::set_target_temp_c_persistent(temp) {
                        Ok(()) => {
                            println!(
                                "http: target temperature set and saved: {:.1}C from {:?}",
                                temp, remote
                            );
                            (
                                "200 OK",
                                "application/json",
                                "no-store",
                                ResponseBody::Owned(temperature_ok_json(temp)),
                            )
                        }
                        Err(error) => {
                            println!(
                                "http: failed to persist setpoint from {:?}: {:?}",
                                remote, error
                            );
                            (
                                "500 Internal Server Error",
                                "application/json",
                                "no-store",
                                ResponseBody::Static("{\n  \"error\": \"persist_failed\"\n}\n"),
                            )
                        }
                    }
                }
            }
            ParsedRequest::PostProbeName(name) => match status::set_temp_probe_name(&name) {
                Ok(()) => {
                    println!("http: probe name set to '{}' from {:?}", name, remote);
                    (
                        "200 OK",
                        "application/json",
                        "no-store",
                        ResponseBody::Owned(probe_name_ok_json(&status::temp_probe_name())),
                    )
                }
                Err(status::ProbeNameError::Empty) => (
                    "400 Bad Request",
                    "application/json",
                    "no-store",
                    ResponseBody::Static("{\n  \"error\": \"empty_probe_name\"\n}\n"),
                ),
                Err(status::ProbeNameError::TooLong) => (
                    "400 Bad Request",
                    "application/json",
                    "no-store",
                    ResponseBody::Static("{\n  \"error\": \"probe_name_too_long\"\n}\n"),
                ),
                Err(status::ProbeNameError::InvalidChar) => (
                    "400 Bad Request",
                    "application/json",
                    "no-store",
                    ResponseBody::Static(
                        "{\n  \"error\": \"invalid_probe_name\", \"allowed\": \"[A-Za-z0-9 ._-]\"\n}\n",
                    ),
                ),
            },
            ParsedRequest::PostCollection(enabled) => {
                status::set_collection_enabled(enabled);
                if !enabled {
                    println!("http: collection stopped from {:?}", remote);
                } else {
                    println!("http: collection started from {:?}", remote);
                }
                (
                    "200 OK",
                    "application/json",
                    "no-store",
                    ResponseBody::Owned(collection_ok_json(enabled)),
                )
            }
            ParsedRequest::PostHistoryClear => match status::clear_history_persistent() {
                Ok(()) => (
                    "200 OK",
                    "application/json",
                    "no-store",
                    ResponseBody::Static("{\n  \"ok\": true\n}\n"),
                ),
                Err(error) => {
                    println!(
                        "http: failed to clear history from {:?}: {:?}",
                        remote, error
                    );
                    (
                        "500 Internal Server Error",
                        "application/json",
                        "no-store",
                        ResponseBody::Static("{\n  \"error\": \"history_clear_failed\"\n}\n"),
                    )
                }
            },
            ParsedRequest::BadRequest => (
                "400 Bad Request",
                "application/json",
                "no-store",
                ResponseBody::Static("{\n  \"error\": \"bad_request\"\n}\n"),
            ),
            ParsedRequest::NotFound => {
                println!("http: 404 for {:?}", remote);
                (
                    "404 Not Found",
                    "application/json",
                    "no-store",
                    ResponseBody::Static("{\n  \"error\": \"not_found\"\n}\n"),
                )
            }
        };

        if !status_line.starts_with("200") {
            status::http_exchange_mark_error();
        }

        if let Err(error) = socket_write_http_response(
            &mut socket,
            status_line,
            content_type,
            cache_control,
            body.as_str(),
        )
        .await
        {
            println!("http: write failed to {:?}: {:?}", remote, error);
            status::http_exchange_mark_error();
            socket.abort();
            let _ = socket.flush().await;
            status::http_exchange_end();
            continue;
        }

        socket.close();
        let _ = socket.flush().await;
        status::http_exchange_end();
    }
}
