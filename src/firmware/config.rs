// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

pub const CONTROL_PERIOD_MS: u64 = 1_000;
pub const STATUS_PRINT_EVERY_SECONDS_DEFAULT: u64 = 5;

/// DS18B20 probe resolution in bits (9, 10, 11, or 12).
/// Configured via `DS18B20_RESOLUTION` in `config.local.toml`.
/// Default is 11-bit (0.125 °C resolution, 375 ms conversion time).
const DS18B20_RESOLUTION_DEFAULT: u8 = 11;
pub const DS18B20_RESOLUTION_CONFIG: Option<&str> = option_env!("DS18B20_RESOLUTION");

/// Return the configured probe resolution, clamped to the valid range [9, 12].
pub fn ds18b20_resolution_bits() -> u8 {
    DS18B20_RESOLUTION_CONFIG
        .and_then(|v| v.parse::<u8>().ok())
        .map(|v| v.clamp(9, 12))
        .unwrap_or(DS18B20_RESOLUTION_DEFAULT)
}

/// Conversion wait time in milliseconds for the configured resolution.
///
/// | Resolution | Conversion time |
/// |------------|-----------------|
/// | 9-bit      |  93.75 ms       |
/// | 10-bit     | 187.5  ms       |
/// | 11-bit     | 375    ms       |
/// | 12-bit     | 750    ms       |
pub fn ds18b20_conversion_ms() -> u64 {
    match ds18b20_resolution_bits() {
        9 => 94,   // ceiling of 93.75 ms
        10 => 188, // ceiling of 187.5 ms
        11 => 375,
        _ => 750, // 12-bit (default max)
    }
}
pub const SSR_WINDOW_STEPS: u32 = 10;
pub const BOOT_OK_DISPLAY_MS: u64 = 2_000;
pub const STATUS_PRINT_EVERY_SECONDS: Option<&str> = option_env!("STATUS_PRINT_EVERY_SECONDS");

/// Heat SSR deadband in °C.  The heating relay turns on when the temperature
/// falls more than this amount below the target, and turns off once the
/// temperature reaches the target.  Configured via `SSR_HEAT_DEADBAND` in
/// `config.local.toml`.  Default: 0.5 °C.
pub const SSR_HEAT_DEADBAND_CONFIG: Option<&str> = option_env!("SSR_HEAT_DEADBAND");
const SSR_HEAT_DEADBAND_DEFAULT: f32 = 0.5;

pub fn ssr_heat_deadband_c() -> f32 {
    SSR_HEAT_DEADBAND_CONFIG
        .and_then(|v| v.parse::<f32>().ok())
        .filter(|&v| v >= 0.0)
        .unwrap_or(SSR_HEAT_DEADBAND_DEFAULT)
}

pub const PID_OUTPUT_LIMIT_PERCENT: f32 = 100.0;
pub const PID_KP: f32 = 14.0;
pub const PID_KI: f32 = 0.35;
pub const PID_KD: f32 = 6.0;

pub const DEVICE_HOSTNAME_DEFAULT: &str = "brewster";
pub const DEVICE_HOSTNAME_CONFIG: Option<&str> = option_env!("DEVICE_HOSTNAME");
pub const TEMP_PROBE_NAME_DEFAULT: &str = "probe-1";
pub const TEMP_PROBE_NAME_CONFIG: Option<&str> = option_env!("TEMP_PROBE_NAME");

pub const WS2812_T0H_TICKS: u16 = 4;
pub const WS2812_T0L_TICKS: u16 = 9;
pub const WS2812_T1H_TICKS: u16 = 8;
pub const WS2812_T1L_TICKS: u16 = 5;

/// Sensor configuration: GPIO pin and human-readable probe name.
/// Configure these in config.local.toml under [[sensors]].
/// Currently, the firmware controls the first sensor (index 0).
/// Additional sensors are read-only on the dashboard.
pub struct SensorConfig {
    #[allow(dead_code)]
    pub pin: u8,
    pub name: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/sensors_config.rs"));

pub fn status_print_every_seconds() -> u64 {
    match STATUS_PRINT_EVERY_SECONDS
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&v| v > 0)
    {
        Some(v) => v,
        None => STATUS_PRINT_EVERY_SECONDS_DEFAULT,
    }
}

pub fn status_print_interval_cycles() -> u32 {
    let seconds = status_print_every_seconds();
    let cycles = seconds
        .saturating_mul(1_000)
        .saturating_add(CONTROL_PERIOD_MS.saturating_sub(1))
        / CONTROL_PERIOD_MS;
    cycles.max(1).min(u32::MAX as u64) as u32
}

pub fn device_hostname() -> &'static str {
    let raw = match DEVICE_HOSTNAME_CONFIG {
        Some(v) if !v.is_empty() => v,
        _ => DEVICE_HOSTNAME_DEFAULT,
    };
    // Enforce the 20-byte wire-format limit.
    if raw.len() <= 20 {
        raw
    } else {
        let mut end = 20;
        while !raw.is_char_boundary(end) {
            end -= 1;
        }
        &raw[..end]
    }
}

/// Maximum byte length of the hostname field in UDP telemetry packets.
pub const HOSTNAME_MAX_LEN: usize = 20;

/// Return the device hostname as a null-padded `[u8; 20]` array for
/// embedding in the UDP wire format.  Truncates silently if longer than 20 bytes.
pub fn device_hostname_bytes() -> [u8; HOSTNAME_MAX_LEN] {
    let mut out = [0u8; HOSTNAME_MAX_LEN];
    let src = device_hostname().as_bytes();
    let len = src.len().min(HOSTNAME_MAX_LEN);
    out[..len].copy_from_slice(&src[..len]);
    out
}

pub fn temp_probe_name() -> &'static str {
    match TEMP_PROBE_NAME_CONFIG {
        Some(v) if !v.is_empty() => v,
        _ => TEMP_PROBE_NAME_DEFAULT,
    }
}

// ── UDP telemetry server ──────────────────────────────────────────────────────

/// IP address of the LAN server that receives UDP telemetry.
/// Set `UDP_SERVER_IP = "x.x.x.x"` in `config.local.toml` to enable.
pub const UDP_SERVER_IP_CONFIG: Option<&str> = option_env!("UDP_SERVER_IP");

/// UDP port the LAN server listens on.  Defaults to 47890 if not set.
pub const UDP_SERVER_PORT_CONFIG: Option<&str> = option_env!("UDP_SERVER_PORT");

pub const UDP_SERVER_PORT_DEFAULT: u16 = 47890;

pub fn udp_server_port() -> u16 {
    match UDP_SERVER_PORT_CONFIG
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|&p| p > 1024)
    {
        Some(p) => p,
        None => UDP_SERVER_PORT_DEFAULT,
    }
}
