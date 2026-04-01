// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

use alloc::string::String;

pub const NTP_MAX_CONFIG_SERVERS: usize = 4;

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NtpSource {
    Unknown = 0,
    Config = 1,
    DhcpGateway = 2,
}

impl NtpSource {
    pub fn from_u8(code: u8) -> Self {
        match code {
            1 => Self::Config,
            2 => Self::DhcpGateway,
            _ => Self::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Config => "config",
            Self::DhcpGateway => "dhcp_gateway",
        }
    }
}

#[derive(Clone, Copy)]
pub struct NtpSelectionSample {
    pub stratum: u8,
    pub latency_ms: u32,
    pub jitter_ms: u32,
}

pub fn parse_ipv4_octets(s: &str) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut i = 0usize;
    for part in s.split('.') {
        if i >= 4 {
            return None;
        }
        octets[i] = part.parse().ok()?;
        i += 1;
    }
    if i == 4 { Some(octets) } else { None }
}

pub fn ntp_config_peers_from(
    config_list: Option<&str>,
    fallback_single: Option<&str>,
) -> heapless::Vec<[u8; 4], NTP_MAX_CONFIG_SERVERS> {
    let mut peers = heapless::Vec::<[u8; 4], NTP_MAX_CONFIG_SERVERS>::new();

    if let Some(list) = config_list.filter(|s| !s.is_empty()) {
        for raw in list.split(',') {
            let candidate = raw.trim();
            if candidate.is_empty() {
                continue;
            }
            let Some(ip) = parse_ipv4_octets(candidate) else {
                continue;
            };
            if peers.contains(&ip) {
                continue;
            }
            if peers.push(ip).is_err() {
                break;
            }
        }
    }

    if peers.is_empty()
        && let Some(ip) = fallback_single
            .filter(|s| !s.is_empty())
            .and_then(parse_ipv4_octets)
    {
        let _ = peers.push(ip);
    }

    peers
}

pub fn should_replace_master(current: NtpSelectionSample, candidate: NtpSelectionSample) -> bool {
    candidate.stratum < current.stratum
        || (candidate.stratum == current.stratum && candidate.latency_ms < current.latency_ms)
        || (candidate.stratum == current.stratum
            && candidate.latency_ms == current.latency_ms
            && candidate.jitter_ms < current.jitter_ms)
}

pub fn normalized_dhcp_hostname(input: &str) -> heapless::String<32> {
    let mut out = heapless::String::<32>::new();

    for byte in input.bytes() {
        if out.len() >= 32 {
            break;
        }

        let mapped = match byte {
            b'A'..=b'Z' => byte + (b'a' - b'A'),
            b'a'..=b'z' | b'0'..=b'9' | b'-' => byte,
            _ => b'-',
        };

        if out.is_empty() && mapped == b'-' {
            continue;
        }

        let _ = out.push(mapped as char);
    }

    while out.as_bytes().last() == Some(&b'-') {
        out.pop();
    }

    if out.is_empty() {
        let _ = out.push_str("brewster");
    }

    out
}

pub fn crc8_maxim(bytes: &[u8]) -> u8 {
    let mut crc = 0u8;

    for &byte in bytes {
        let mut value = byte;

        for _ in 0..8 {
            let mix = (crc ^ value) & 0x01;
            crc >>= 1;
            if mix != 0 {
                crc ^= 0x8C;
            }
            value >>= 1;
        }
    }

    crc
}

pub fn days_to_ymd(z: u32) -> (u32, u32, u32) {
    let z = z as i64 + 719_468;
    let era = if z >= 0 {
        z / 146_097
    } else {
        (z - 146_096) / 146_097
    };
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe as i64 + era * 400 + if m <= 2 { 1 } else { 0 };
    (y as u32, m as u32, d as u32)
}

pub fn unix_to_iso8601(secs: u32) -> String {
    let day_secs = secs % 86_400;
    let days = secs / 86_400;
    let h = day_secs / 3_600;
    let m = (day_secs % 3_600) / 60;
    let s = day_secs % 60;
    let (year, month, day) = days_to_ymd(days);
    alloc::format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        month,
        day,
        h,
        m,
        s
    )
}
