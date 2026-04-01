// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

use alloc::string::ToString;
use embassy_net::Stack;
use embassy_time::{Duration, Timer, with_timeout};
use esp_radio::wifi::{WifiController, WifiDevice};

const WIFI_CONNECT_TIMEOUT_SECS: u64 = 20;
const WIFI_DHCP_WAIT_TIMEOUT_SECS: u64 = 30;
const WIFI_SCAN_TIMEOUT_SECS: u64 = 12;
const WIFI_SCAN_EVERY_ATTEMPTS_DEFAULT: u32 = 6;
const WIFI_SCAN_EVERY_ATTEMPTS: Option<&str> = option_env!("WIFI_SCAN_EVERY_ATTEMPTS");

pub fn scan_every_attempts() -> u32 {
    match WIFI_SCAN_EVERY_ATTEMPTS
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|&v| v > 0)
    {
        Some(v) => v,
        None => WIFI_SCAN_EVERY_ATTEMPTS_DEFAULT,
    }
}

#[embassy_executor::task]
#[allow(
    clippy::large_stack_frames,
    reason = "wifi connection task keeps scan/connect futures and state on stack"
)]
pub async fn wifi_connection_task(
    mut controller: WifiController<'static>,
    stack: Stack<'static>,
    scan_interval_attempts: u32,
) {
    esp_println::println!("wifi: connection task started");
    let mut connect_attempts = 0u32;

    loop {
        match controller.is_started() {
            Ok(true) => {}
            Ok(false) => {
                esp_println::println!("wifi: starting controller...");
                match with_timeout(Duration::from_secs(10), controller.start_async()).await {
                    Ok(Ok(())) => {
                        esp_println::println!("wifi: controller started");
                    }
                    Ok(Err(error)) => {
                        esp_println::println!("wifi: controller start failed: {:?}", error);
                        Timer::after(Duration::from_secs(2)).await;
                        continue;
                    }
                    Err(_) => {
                        esp_println::println!("wifi: controller start timeout");
                        Timer::after(Duration::from_secs(2)).await;
                        continue;
                    }
                }
            }
            Err(error) => {
                esp_println::println!("wifi: controller state check failed: {:?}", error);
                Timer::after(Duration::from_secs(2)).await;
                continue;
            }
        }

        if matches!(controller.is_connected(), Ok(true)) {
            connect_attempts = 0;
            Timer::after(Duration::from_secs(5)).await;
            continue;
        }

        connect_attempts = connect_attempts.saturating_add(1);
        let should_scan = !stack.is_link_up()
            && (connect_attempts == 1
                || connect_attempts.is_multiple_of(scan_interval_attempts));
        if should_scan {
            esp_println::println!("wifi: scanning nearby SSIDs...");
            match with_timeout(
                Duration::from_secs(WIFI_SCAN_TIMEOUT_SECS),
                controller.scan_with_config_async(esp_radio::wifi::ScanConfig::default()),
            )
            .await
            {
                Ok(Ok(aps)) => {
                    esp_println::println!("wifi: scan found {} AP(s)", aps.len());
                    for ap in aps.iter().take(8) {
                        let ssid = if ap.ssid.is_empty() {
                            "<hidden>"
                        } else {
                            ap.ssid.as_str()
                        };
                        esp_println::println!(
                            "wifi: ap ssid='{}' ch={} rssi={} auth={:?}",
                            ssid, ap.channel, ap.signal_strength, ap.auth_method
                        );
                    }
                }
                Ok(Err(error)) => {
                    esp_println::println!("wifi: scan failed: {:?}", error);
                }
                Err(_) => {
                    esp_println::println!("wifi: scan timeout");
                }
            }
        }

        esp_println::println!("wifi: connecting...");

        match with_timeout(
            Duration::from_secs(WIFI_CONNECT_TIMEOUT_SECS),
            controller.connect_async(),
        )
        .await
        {
            Ok(Ok(())) => {
                esp_println::println!("wifi: connected to access point");
            }
            Ok(Err(error)) => {
                esp_println::println!("wifi: connect failed: {:?}", error);
            }
            Err(_) => {
                esp_println::println!("wifi: connect timeout (check SSID/password and 2.4GHz AP)");
            }
        }

        Timer::after(Duration::from_secs(5)).await;
    }
}

#[embassy_executor::task]
pub async fn wifi_net_task(mut runner: embassy_net::Runner<'static, WifiDevice<'static>>) {
    runner.run().await
}

#[embassy_executor::task]
pub async fn wifi_status_task(stack: Stack<'static>) {
    esp_println::println!("wifi: status task started");
    let mut last_link_up: Option<bool> = None;
    let mut wait_log_ticks = 0u8;

    loop {
        match with_timeout(
            Duration::from_secs(WIFI_DHCP_WAIT_TIMEOUT_SECS),
            stack.wait_config_up(),
        )
        .await
        {
            Ok(()) => {
                if let Some(config) = stack.config_v4() {
                    esp_println::println!("wifi: got IPv4 address {}", config.address);
                    crate::firmware::status::update_ip_from_cidr(&config.address.to_string());
                    if let Some(gateway) = config.gateway {
                        crate::firmware::status::update_dhcp_ntp_server(gateway.octets());
                    }
                }

                wait_log_ticks = 0;

                stack.wait_config_down().await;
                crate::firmware::status::clear_ip();
                esp_println::println!("wifi: network configuration lost");
                last_link_up = Some(false);
            }
            Err(_) => {
                let link_up = stack.is_link_up();
                if link_up {
                    crate::firmware::status::mark_net_dhcp_pending();
                } else {
                    crate::firmware::status::mark_net_link_down();
                }

                wait_log_ticks = wait_log_ticks.saturating_add(1);
                let state_changed = last_link_up != Some(link_up);
                // Emit wait diagnostics only on state transitions or every 30s.
                if state_changed || wait_log_ticks >= 6 {
                    wait_log_ticks = 0;
                    if link_up {
                        esp_println::println!(
                            "wifi: link up, waiting up to {}s for DHCP lease...",
                            WIFI_DHCP_WAIT_TIMEOUT_SECS
                        );
                    } else {
                        esp_println::println!("wifi: waiting for AP link...");
                    }
                }
                last_link_up = Some(link_up);
            }
        }
    }
}
