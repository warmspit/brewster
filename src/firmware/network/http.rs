// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

use core::fmt::Write as _;
use embassy_net::Stack;
use embassy_net::tcp::{Error as TcpError, TcpSocket};
use embassy_time::{Duration, Timer};
use esp_println::println;

use crate::firmware::{metrics, status};

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

#[derive(Clone, Copy)]
enum ParsedRequest {
    GetStatus,
    GetMetrics,
    PostTemperature(f32),
    BadRequest,
    NotFound,
}

fn parse_request(buf: &[u8]) -> ParsedRequest {
    if buf.starts_with(b"GET / ")
        || buf.starts_with(b"GET /status ")
        || buf.starts_with(b"GET /status?")
        || buf.starts_with(b"GET /status\r")
    {
        return ParsedRequest::GetStatus;
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
    body: &str,
) -> Result<(), TcpError> {
    let mut header = alloc::string::String::with_capacity(96);
    let _ = write!(
        header,
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status_line,
        content_type,
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
                socket.abort();
                let _ = socket.flush().await;
                continue;
            }
            Err(error) => {
                println!("http: read failed from {:?}: {:?}", remote, error);
                socket.abort();
                let _ = socket.flush().await;
                continue;
            }
        };

        let (status_line, content_type, body) = match parsed {
            ParsedRequest::GetStatus => {
                status::http_request_received();
                println!("http: serving status to {:?}", remote);
                (
                    "200 OK",
                    "application/json",
                    ResponseBody::Owned(metrics::json()),
                )
            }
            ParsedRequest::GetMetrics => {
                status::http_request_received();
                println!("http: serving metrics to {:?}", remote);
                (
                    "200 OK",
                    "text/plain; version=0.0.4; charset=utf-8",
                    ResponseBody::Owned(metrics::prometheus()),
                )
            }
            ParsedRequest::PostTemperature(temp) => {
                status::http_request_received();
                if !(0.0_f32..=150.0).contains(&temp) {
                    (
                        "400 Bad Request",
                        "application/json",
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
                                ResponseBody::Static("{\n  \"error\": \"persist_failed\"\n}\n"),
                            )
                        }
                    }
                }
            }
            ParsedRequest::BadRequest => (
                "400 Bad Request",
                "application/json",
                ResponseBody::Static("{\n  \"error\": \"bad_request\"\n}\n"),
            ),
            ParsedRequest::NotFound => {
                println!("http: 404 for {:?}", remote);
                (
                    "404 Not Found",
                    "application/json",
                    ResponseBody::Static("{\n  \"error\": \"not_found\"\n}\n"),
                )
            }
        };

        if let Err(error) =
            socket_write_http_response(&mut socket, status_line, content_type, body.as_str()).await
        {
            println!("http: write failed to {:?}: {:?}", remote, error);
            socket.abort();
            let _ = socket.flush().await;
            continue;
        }

        socket.close();
        let _ = socket.flush().await;
    }
}
