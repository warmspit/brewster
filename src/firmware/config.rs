// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

pub const CONTROL_PERIOD_MS: u64 = 1_000;
pub const DS18B20_CONVERSION_MS: u64 = 750;
pub const STATUS_PRINT_EVERY_SECONDS_DEFAULT: u64 = 5;
pub const SSR_WINDOW_STEPS: u32 = 10;
pub const BOOT_OK_DISPLAY_MS: u64 = 2_000;
pub const STATUS_PRINT_EVERY_SECONDS: Option<&str> = option_env!("STATUS_PRINT_EVERY_SECONDS");

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
    match DEVICE_HOSTNAME_CONFIG {
        Some(v) if !v.is_empty() => v,
        _ => DEVICE_HOSTNAME_DEFAULT,
    }
}

pub fn temp_probe_name() -> &'static str {
    match TEMP_PROBE_NAME_CONFIG {
        Some(v) if !v.is_empty() => v,
        _ => TEMP_PROBE_NAME_DEFAULT,
    }
}
