// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

use alloc::string::ToString;
use core::fmt::Write as _;
use embassy_net::Stack;
use embassy_net::tcp::{Error as TcpError, TcpSocket};
use embassy_time::{Duration, Timer};
use esp_println::println;

use crate::firmware::{metrics, sensor, status};

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

fn config_json(http: bool, prometheus: bool) -> alloc::string::String {
    let mut body = alloc::string::String::with_capacity(80);
    let _ = write!(
        body,
        concat!(
            "{{\n",
            "  \"http_server\": {},\n",
            "  \"prometheus\": {}\n",
            "}}\n"
        ),
        if http { "true" } else { "false" },
        if prometheus { "true" } else { "false" },
    );
    body
}

fn sensor_scan_json() -> alloc::string::String {
    let mut body = alloc::string::String::with_capacity(768);
    let scan = status::sensor_scan_snapshot();
    let _ = write!(
        body,
        "{{\n  \"ok\": true,\n  \"last_scan_uptime_s\": {},\n  \"found\": [",
        status::sensor_scan_last_uptime_s()
    );

    for (idx, rom) in scan.iter().copied().enumerate() {
        if idx > 0 {
            body.push_str(",");
        }
        let serial = sensor::format_ds18b20_serial(rom);
        let mapped_name = crate::firmware::config::SENSORS.iter().find_map(|cfg| {
            let parsed = cfg.serial.and_then(sensor::parse_ds18b20_serial)?;
            if parsed == rom { Some(cfg.name) } else { None }
        });
        if let Some(name) = mapped_name {
            let _ = write!(
                body,
                "\n    {{ \"serial\": \"{}\", \"name\": \"{}\" }}",
                serial, name
            );
        } else {
            let _ = write!(
                body,
                "\n    {{ \"serial\": \"{}\", \"name\": null }}",
                serial
            );
        }
    }
    if !scan.is_empty() {
        body.push('\n');
    }
    body.push_str("  ]\n}\n");
    body
}

/// Parse `{"http_server": true/false, "prometheus": true/false}` from the request body.
fn parse_config_body(buf: &[u8]) -> Option<(bool, bool)> {
    let header_end = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let body = core::str::from_utf8(&buf[header_end + 4..]).ok()?.trim();
    let http = parse_json_bool_field(body, "http_server")?;
    let prometheus = parse_json_bool_field(body, "prometheus")?;
    Some((http, prometheus))
}

/// Minimal JSON boolean field parser without allocation.
fn parse_json_bool_field(body: &str, key: &str) -> Option<bool> {
    let key_bytes = key.as_bytes();
    let needle_len = key_bytes.len() + 2;
    let body_bytes = body.as_bytes();
    let key_pos = body_bytes.windows(needle_len).position(|w| {
        w[0] == b'"' && w[1..needle_len - 1] == *key_bytes && w[needle_len - 1] == b'"'
    })?;
    let after_colon = body[key_pos + needle_len..]
        .splitn(2, ':')
        .nth(1)?
        .trim_start();
    if after_colon.starts_with("true") {
        Some(true)
    } else if after_colon.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

fn parse_json_string_field(body: &str, key: &str) -> Option<alloc::string::String> {
    // Scan for `"<key>"` without allocating a pattern String.
    let body_bytes = body.as_bytes();
    let key_bytes = key.as_bytes();
    let needle_len = key_bytes.len() + 2; // opening `"` + key + closing `"`
    let key_pos = body_bytes.windows(needle_len).position(|w| {
        w[0] == b'"' && w[1..needle_len - 1] == *key_bytes && w[needle_len - 1] == b'"'
    })?;
    let after_key = &body[key_pos + needle_len..];
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
    GetStatus,
    GetHistory(usize),
    GetSensorScan,
    GetConfig,
    #[cfg(feature = "prometheus")]
    GetMetrics,
    PostTemperature(f32),
    PostCollection(bool),
    PostHistoryClear,
    PostProbeName(alloc::string::String),
    PostConfig {
        http: bool,
        prometheus: bool,
    },
    // Temperature profile endpoints
    GetProfiles,
    GetProfile(alloc::string::String),
    GetActiveProfile,
    PostProfile {
        name: alloc::string::String,
        steps: heapless::Vec<(f32, u32), { status::MAX_STEPS_PER_PROFILE }>,
    },
    DeleteProfile(alloc::string::String),
    StartProfile(alloc::string::String),
    StopProfile,
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

#[allow(
    clippy::large_stack_frames,
    reason = "parse_request holds ParsedRequest variants on the stack; the enum is unavoidably large because PostProbeName carries a heap-allocated String"
)]
fn parse_request(buf: &[u8]) -> ParsedRequest {
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

    if buf.starts_with(b"GET /sensors/scan ")
        || buf.starts_with(b"GET /sensors/scan?")
        || buf.starts_with(b"GET /sensors/scan\r")
    {
        return ParsedRequest::GetSensorScan;
    }

    #[cfg(feature = "prometheus")]
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

    if buf.starts_with(b"GET /config ") || buf.starts_with(b"GET /config\r") {
        return ParsedRequest::GetConfig;
    }

    if buf.starts_with(b"POST /config ") || buf.starts_with(b"POST /config\r") {
        return match parse_config_body(buf) {
            Some((http, prometheus)) => ParsedRequest::PostConfig { http, prometheus },
            None => ParsedRequest::BadRequest,
        };
    }

    // ── Temperature profile endpoints ─────────────────────────────────────

    if buf.starts_with(b"GET /profiles ")
        || buf.starts_with(b"GET /profiles\r")
        || buf.starts_with(b"GET /profiles?")
    {
        return ParsedRequest::GetProfiles;
    }

    if buf.starts_with(b"GET /profiles/active ") || buf.starts_with(b"GET /profiles/active\r") {
        return ParsedRequest::GetActiveProfile;
    }

    if buf.starts_with(b"POST /profiles/stop ") || buf.starts_with(b"POST /profiles/stop\r") {
        return ParsedRequest::StopProfile;
    }

    // POST /profiles/<name>/start
    if buf.starts_with(b"POST /profiles/") {
        if let Some(name) = parse_url_segment_before(buf, b"POST /profiles/", b"/start ") {
            return ParsedRequest::StartProfile(name);
        }
    }

    // GET /profiles/<name>
    if buf.starts_with(b"GET /profiles/") {
        if let Some(name) = parse_url_name(buf, b"GET /profiles/") {
            return ParsedRequest::GetProfile(name);
        }
        return ParsedRequest::BadRequest;
    }

    // POST /profiles/<name>  (create / replace)
    if buf.starts_with(b"POST /profiles/") {
        let header_end = match buf.windows(4).position(|w| w == b"\r\n\r\n") {
            Some(i) => i,
            None => return ParsedRequest::BadRequest,
        };
        let body = match core::str::from_utf8(&buf[header_end + 4..]) {
            Ok(s) => s.trim(),
            Err(_) => return ParsedRequest::BadRequest,
        };
        let name = match parse_url_name(buf, b"POST /profiles/") {
            Some(n) => n,
            None => return ParsedRequest::BadRequest,
        };
        let steps = match parse_profile_steps(body) {
            Some(s) => s,
            None => return ParsedRequest::BadRequest,
        };
        return ParsedRequest::PostProfile { name, steps };
    }

    // DELETE /profiles/<name>
    if buf.starts_with(b"DELETE /profiles/") {
        if let Some(name) = parse_url_name(buf, b"DELETE /profiles/") {
            return ParsedRequest::DeleteProfile(name);
        }
        return ParsedRequest::BadRequest;
    }

    ParsedRequest::NotFound
}

/// Extract the URL segment after `prefix` up to the next `/`, space, `?`, or `\r`.
fn parse_url_name(buf: &[u8], prefix: &[u8]) -> Option<alloc::string::String> {
    if !buf.starts_with(prefix) {
        return None;
    }
    let rest = &buf[prefix.len()..];
    let end = rest
        .iter()
        .position(|&b| matches!(b, b'/' | b' ' | b'?' | b'\r' | b'\n'))
        .unwrap_or(rest.len());
    let name = core::str::from_utf8(&rest[..end]).ok()?.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Extract the URL segment between `prefix` and `suffix`.
fn parse_url_segment_before(
    buf: &[u8],
    prefix: &[u8],
    suffix: &[u8],
) -> Option<alloc::string::String> {
    if !buf.starts_with(prefix) {
        return None;
    }
    let rest = &buf[prefix.len()..];
    let end = rest.windows(suffix.len()).position(|w| w == suffix)?;
    let name = core::str::from_utf8(&rest[..end]).ok()?.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Parse `"target_c"` (f32) from a JSON object fragment.
fn parse_json_f32_field(body: &str, key: &str) -> Option<f32> {
    let key_bytes = key.as_bytes();
    let needle_len = key_bytes.len() + 2;
    let body_bytes = body.as_bytes();
    let key_pos = body_bytes.windows(needle_len).position(|w| {
        w[0] == b'"' && w[1..needle_len - 1] == *key_bytes && w[needle_len - 1] == b'"'
    })?;
    let after = &body[key_pos + needle_len..];
    let after_colon = after.splitn(2, ':').nth(1)?.trim_start();
    let end = after_colon
        .find(|c: char| matches!(c, ',' | '}' | ' ' | '\t' | '\r' | '\n'))
        .unwrap_or(after_colon.len());
    after_colon[..end].parse::<f32>().ok()
}

/// Parse `"hold_secs"` (u32) from a JSON object fragment.
fn parse_json_u32_field(body: &str, key: &str) -> Option<u32> {
    let key_bytes = key.as_bytes();
    let needle_len = key_bytes.len() + 2;
    let body_bytes = body.as_bytes();
    let key_pos = body_bytes.windows(needle_len).position(|w| {
        w[0] == b'"' && w[1..needle_len - 1] == *key_bytes && w[needle_len - 1] == b'"'
    })?;
    let after = &body[key_pos + needle_len..];
    let after_colon = after.splitn(2, ':').nth(1)?.trim_start();
    let end = after_colon
        .find(|c: char| matches!(c, ',' | '}' | ' ' | '\t' | '\r' | '\n'))
        .unwrap_or(after_colon.len());
    after_colon[..end].parse::<u32>().ok()
}

/// Parse the `"steps": [...]` array from a profile POST body.
/// Each element must contain `target_c` (f32) and `hold_secs` (u32).
fn parse_profile_steps(
    body: &str,
) -> Option<heapless::Vec<(f32, u32), { status::MAX_STEPS_PER_PROFILE }>> {
    let steps_pos = body.find("\"steps\"")?;
    let after_steps = &body[steps_pos + 7..];
    let colon_pos = after_steps.find(':')?;
    let after_colon = after_steps[colon_pos + 1..].trim_start();
    if !after_colon.starts_with('[') {
        return None;
    }
    let mut cursor = &after_colon[1..]; // skip '['
    let mut steps = heapless::Vec::new();

    loop {
        cursor = cursor.trim_start();
        if cursor.starts_with(']') {
            break;
        }
        if cursor.starts_with(',') {
            cursor = &cursor[1..];
            continue;
        }
        if !cursor.starts_with('{') {
            return None;
        }
        let end = cursor.find('}')?;
        let obj = &cursor[1..end];
        cursor = &cursor[end + 1..];

        let target_c = parse_json_f32_field(obj, "target_c")?;
        let hold_secs = parse_json_u32_field(obj, "hold_secs")?;

        if !(-20.0_f32..=100.0).contains(&target_c) || hold_secs == 0 {
            return None;
        }
        if steps.push((target_c, hold_secs)).is_err() {
            return None; // too many steps
        }
    }
    if steps.is_empty() { None } else { Some(steps) }
}

/// Serialize all stored profiles as a JSON list.
fn profiles_list_json() -> alloc::string::String {
    use core::fmt::Write as _;
    let mut body = alloc::string::String::with_capacity(512);
    body.push_str("{\n  \"profiles\": [");
    let mut first = true;
    for slot in 0..status::MAX_PROFILES {
        if let Some(p) = status::profile_load(slot) {
            if !first {
                body.push(',');
            }
            first = false;
            let _ = write!(
                body,
                "\n    {{ \"slot\": {}, \"name\": \"{}\", \"steps\": {} }}",
                slot,
                p.name,
                p.steps.len()
            );
        }
    }
    if !first {
        body.push('\n');
    }
    body.push_str("  ]\n}\n");
    body
}

/// Serialize one profile as JSON.
fn profile_json(slot: usize, profile: &status::TempProfile) -> alloc::string::String {
    use core::fmt::Write as _;
    let mut body = alloc::string::String::with_capacity(512);
    let _ = write!(
        body,
        concat!(
            "{{\n",
            "  \"ok\": true,\n",
            "  \"slot\": {},\n",
            "  \"name\": \"{}\",\n",
            "  \"steps\": ["
        ),
        slot, profile.name,
    );
    for (i, step) in profile.steps.iter().enumerate() {
        if i > 0 {
            body.push(',');
        }
        let _ = write!(
            body,
            "\n    {{ \"target_c\": {:.2}, \"target_f\": {:.2}, \"hold_secs\": {} }}",
            step.target_c,
            step.target_c * 9.0 / 5.0 + 32.0,
            step.hold_secs,
        );
    }
    if !profile.steps.is_empty() {
        body.push('\n');
    }
    body.push_str("  ]\n}\n");
    body
}

/// Serialize the active profile runtime state as JSON.
fn active_profile_json() -> alloc::string::String {
    use core::fmt::Write as _;
    match status::active_profile_state() {
        None => "{\n  \"active\": false\n}\n".to_string(),
        Some(s) => {
            let mut body = alloc::string::String::with_capacity(256);
            let _ = write!(
                body,
                concat!(
                    "{{\n",
                    "  \"active\": true,\n",
                    "  \"name\": \"{}\",\n",
                    "  \"step_index\": {},\n",
                    "  \"total_steps\": {},\n",
                    "  \"step_target_c\": {:.2},\n",
                    "  \"step_target_f\": {:.2},\n",
                    "  \"step_hold_secs\": {},\n",
                    "  \"at_target\": {},\n",
                    "  \"hold_elapsed_secs\": {}\n",
                    "}}\n"
                ),
                s.name,
                s.step_index,
                s.total_steps,
                s.step_target_c,
                s.step_target_c * 9.0 / 5.0 + 32.0,
                s.step_hold_secs,
                if s.at_target { "true" } else { "false" },
                s.hold_elapsed_secs,
            );
            body
        }
    }
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
            ParsedRequest::GetSensorScan => {
                println!("http: serving sensors/scan to {:?}", remote);
                (
                    "200 OK",
                    "application/json",
                    "no-store",
                    ResponseBody::Owned(sensor_scan_json()),
                )
            }
            #[cfg(feature = "prometheus")]
            ParsedRequest::GetMetrics => {
                if !status::feature_prometheus_enabled() {
                    (
                        "404 Not Found",
                        "application/json",
                        "no-store",
                        ResponseBody::Static("{\n  \"error\": \"not_found\"\n}\n"),
                    )
                } else {
                    println!("http: serving metrics to {:?}", remote);
                    (
                        "200 OK",
                        "text/plain; version=0.0.4; charset=utf-8",
                        "no-store",
                        ResponseBody::Owned(metrics::prometheus()),
                    )
                }
            }
            ParsedRequest::GetConfig => {
                let http = status::feature_http_enabled();
                let prom = status::feature_prometheus_enabled();
                (
                    "200 OK",
                    "application/json",
                    "no-store",
                    ResponseBody::Owned(config_json(http, prom)),
                )
            }
            ParsedRequest::PostConfig { http, prometheus } => {
                match status::set_features_persistent(http, prometheus) {
                    Ok(()) => {
                        println!(
                            "http: features updated: http={} prometheus={} from {:?}",
                            http, prometheus, remote
                        );
                        (
                            "200 OK",
                            "application/json",
                            "no-store",
                            ResponseBody::Owned(config_json(http, prometheus)),
                        )
                    }
                    Err(error) => {
                        println!(
                            "http: failed to persist features from {:?}: {:?}",
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
            ParsedRequest::PostTemperature(temp) => {
                if !(-20.0_f32..=100.0).contains(&temp) {
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
                if let Err(e) = status::set_collection_enabled_persistent(enabled) {
                    println!("http: failed to persist collection state: {:?}", e);
                }
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
                Ok(()) => {
                    super::udp::set_history_clear_pending();
                    (
                        "200 OK",
                        "application/json",
                        "no-store",
                        ResponseBody::Static("{\n  \"ok\": true\n}\n"),
                    )
                }
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
            // ── Temperature profile handlers ──────────────────────────────────
            ParsedRequest::GetProfiles => (
                "200 OK",
                "application/json",
                "no-store",
                ResponseBody::Owned(profiles_list_json()),
            ),
            ParsedRequest::GetProfile(name) => match status::profile_find_by_name(&name) {
                Some((slot, profile)) => (
                    "200 OK",
                    "application/json",
                    "no-store",
                    ResponseBody::Owned(profile_json(slot, &profile)),
                ),
                None => (
                    "404 Not Found",
                    "application/json",
                    "no-store",
                    ResponseBody::Static("{\n  \"error\": \"profile_not_found\"\n}\n"),
                ),
            },
            ParsedRequest::GetActiveProfile => (
                "200 OK",
                "application/json",
                "no-store",
                ResponseBody::Owned(active_profile_json()),
            ),
            ParsedRequest::PostProfile { name, steps } => {
                // Find existing slot by name or claim a new one.
                let slot = status::profile_find_by_name(&name)
                    .map(|(s, _)| s)
                    .or_else(|| status::profile_find_empty_slot());
                match slot {
                    None => (
                        "409 Conflict",
                        "application/json",
                        "no-store",
                        ResponseBody::Static("{\n  \"error\": \"profile_slots_full\"\n}\n"),
                    ),
                    Some(slot) => {
                        let mut profile_name: heapless::String<{ status::MAX_PROFILE_NAME_LEN }> =
                            heapless::String::new();
                        let _ = profile_name.push_str(name.trim());
                        let profile_steps: heapless::Vec<
                            status::ProfileStep,
                            { status::MAX_STEPS_PER_PROFILE },
                        > = steps
                            .iter()
                            .map(|&(target_c, hold_secs)| status::ProfileStep {
                                target_c,
                                hold_secs,
                            })
                            .collect();
                        let profile = status::TempProfile {
                            name: profile_name,
                            steps: profile_steps,
                        };
                        match status::profile_save(slot, &profile) {
                            Ok(()) => {
                                println!(
                                    "http: profile '{}' saved in slot {} from {:?}",
                                    name, slot, remote
                                );
                                (
                                    "200 OK",
                                    "application/json",
                                    "no-store",
                                    ResponseBody::Owned(profile_json(slot, &profile)),
                                )
                            }
                            Err(status::ProfileError::InvalidName) => (
                                "400 Bad Request",
                                "application/json",
                                "no-store",
                                ResponseBody::Static(
                                    "{\n  \"error\": \"invalid_profile_name\"\n}\n",
                                ),
                            ),
                            Err(status::ProfileError::InvalidStep) => (
                                "400 Bad Request",
                                "application/json",
                                "no-store",
                                ResponseBody::Static(
                                    "{\n  \"error\": \"invalid_profile_step\"\n}\n",
                                ),
                            ),
                            Err(_) => (
                                "500 Internal Server Error",
                                "application/json",
                                "no-store",
                                ResponseBody::Static(
                                    "{\n  \"error\": \"profile_save_failed\"\n}\n",
                                ),
                            ),
                        }
                    }
                }
            }
            ParsedRequest::DeleteProfile(name) => {
                match status::profile_find_by_name(&name) {
                    None => (
                        "404 Not Found",
                        "application/json",
                        "no-store",
                        ResponseBody::Static("{\n  \"error\": \"profile_not_found\"\n}\n"),
                    ),
                    Some((slot, _)) => {
                        // If this profile is currently active, stop it first.
                        if let Some(state) = status::active_profile_state() {
                            if state.name.as_str().eq_ignore_ascii_case(&name) {
                                status::stop_profile();
                            }
                        }
                        match status::profile_delete(slot) {
                            Ok(()) => {
                                println!(
                                    "http: profile '{}' deleted from slot {} by {:?}",
                                    name, slot, remote
                                );
                                (
                                    "200 OK",
                                    "application/json",
                                    "no-store",
                                    ResponseBody::Static("{\n  \"ok\": true\n}\n"),
                                )
                            }
                            Err(_) => (
                                "500 Internal Server Error",
                                "application/json",
                                "no-store",
                                ResponseBody::Static(
                                    "{\n  \"error\": \"profile_delete_failed\"\n}\n",
                                ),
                            ),
                        }
                    }
                }
            }
            ParsedRequest::StartProfile(name) => match status::start_profile(&name) {
                Ok(()) => {
                    println!("http: profile '{}' started from {:?}", name, remote);
                    let target_c = status::get_target_temp_c();
                    let mut body = alloc::string::String::with_capacity(96);
                    let _ = core::fmt::Write::write_fmt(
                        &mut body,
                        format_args!(
                            "{{\n  \"ok\": true,\n  \"name\": \"{}\",\n  \"step\": 0,\n  \"target_c\": {:.2}\n}}\n",
                            name, target_c
                        ),
                    );
                    (
                        "200 OK",
                        "application/json",
                        "no-store",
                        ResponseBody::Owned(body),
                    )
                }
                Err(status::ProfileError::NotFound) => (
                    "404 Not Found",
                    "application/json",
                    "no-store",
                    ResponseBody::Static("{\n  \"error\": \"profile_not_found\"\n}\n"),
                ),
                Err(status::ProfileError::InvalidStep) => (
                    "400 Bad Request",
                    "application/json",
                    "no-store",
                    ResponseBody::Static("{\n  \"error\": \"profile_has_no_steps\"\n}\n"),
                ),
                Err(_) => (
                    "500 Internal Server Error",
                    "application/json",
                    "no-store",
                    ResponseBody::Static("{\n  \"error\": \"start_profile_failed\"\n}\n"),
                ),
            },
            ParsedRequest::StopProfile => {
                status::stop_profile();
                println!("http: profile stopped from {:?}", remote);
                (
                    "200 OK",
                    "application/json",
                    "no-store",
                    ResponseBody::Static("{\n  \"ok\": true\n}\n"),
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
