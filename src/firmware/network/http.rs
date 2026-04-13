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
            let _ = write!(body, "\n    {{ \"serial\": \"{}\", \"name\": null }}", serial);
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
