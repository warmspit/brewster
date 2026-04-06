// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

use alloc::string::ToString;

#[cfg(feature = "http-server")]
#[path = "network/http.rs"]
mod http;
#[path = "network/mdns.rs"]
mod mdns;
#[path = "network/ntp.rs"]
mod ntp;
#[path = "network/udp.rs"]
mod udp;
#[path = "network/wifi.rs"]
mod wifi;

use embassy_executor::Spawner;
use embassy_net::DhcpConfig;
use embassy_net::Ipv4Address;
use embassy_net::Ipv6Address;
use embassy_net::StackResources;
use embassy_net::udp::PacketMetadata;
use esp_hal::peripherals::WIFI;
use esp_hal::rng::Rng;
use esp_radio::wifi::{ClientConfig, Config as WifiConfig, ModeConfig};
use static_cell::{ConstStaticCell, StaticCell};

use super::shared;

#[cfg(feature = "http-server")]
const HTTP_PORT: u16 = 80;
const MDNS_PORT: u16 = 5353;
const MDNS_MULTICAST: Ipv4Address = Ipv4Address::new(224, 0, 0, 251);
const MDNS_MULTICAST_V6: Ipv6Address = Ipv6Address::new(0xff02, 0, 0, 0, 0, 0, 0, 0x00fb);

#[cfg(feature = "http-server")]
static HTTP_RX_BUFFER: ConstStaticCell<[u8; 1024]> = ConstStaticCell::new([0; 1024]);
#[cfg(feature = "http-server")]
static HTTP_TX_BUFFER: ConstStaticCell<[u8; 1024]> = ConstStaticCell::new([0; 1024]);
static MDNS_RX_META: ConstStaticCell<[PacketMetadata; 4]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; 4]);
static MDNS_TX_META: ConstStaticCell<[PacketMetadata; 4]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; 4]);
static MDNS_RX_BUFFER: ConstStaticCell<[u8; 768]> = ConstStaticCell::new([0; 768]);
static MDNS_TX_BUFFER: ConstStaticCell<[u8; 768]> = ConstStaticCell::new([0; 768]);
static MDNS_RECV_PACKET: ConstStaticCell<[u8; 512]> = ConstStaticCell::new([0; 512]);
static MDNS_SEND_PACKET: ConstStaticCell<[u8; 512]> = ConstStaticCell::new([0; 512]);

#[allow(
    clippy::large_stack_frames,
    reason = "Wi-Fi bootstrap temporarily holds network driver config before tasks are spawned"
)]
pub fn configure_wifi(spawner: &Spawner, wifi: WIFI<'static>, hostname: &str) {
    let Some(ssid) = option_env!("SSID").filter(|ssid| !ssid.is_empty()) else {
        esp_println::println!("wifi: disabled, set SSID and PASSWORD in config.local.toml [env]");
        return;
    };

    let password = option_env!("PASSWORD").unwrap_or("");

    static RADIO: StaticCell<esp_radio::Controller<'static>> = StaticCell::new();
    let radio = RADIO.init(esp_radio::init().unwrap());

    let client_config = ClientConfig::default()
        .with_ssid(ssid.to_string())
        .with_password(password.to_string());

    esp_println::println!("wifi: starting station mode for ssid={}", ssid);
    let (mut controller, interfaces) =
        esp_radio::wifi::new(radio, wifi, WifiConfig::default()).unwrap();
    esp_println::println!("wifi: radio configured and started");

    controller
        .set_config(&ModeConfig::Client(client_config))
        .unwrap();
    esp_println::println!("wifi: station configuration applied");

    let wifi_interface = interfaces.sta;
    let mut dhcp_config = DhcpConfig::default();
    let dhcp_hostname = normalized_dhcp_hostname(hostname);
    esp_println::println!("wifi: dhcp hostname={}", dhcp_hostname.as_str());
    dhcp_config.hostname = Some(dhcp_hostname);

    let net_config = embassy_net::Config::dhcpv4(dhcp_config);
    let scan_interval_attempts = wifi::scan_every_attempts();
    esp_println::println!(
        "wifi: scan interval set to every {} failed connect attempt(s)",
        scan_interval_attempts
    );
    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;
    // Use a third random draw as the session nonce so it is independent of the
    // network seed (which embassy-net may consume incrementally).
    udp::init_nonce(rng.random());

    static NET_RESOURCES: StaticCell<StackResources<6>> = StaticCell::new();
    let resources = NET_RESOURCES.init(StackResources::<6>::new());

    let (stack, runner) = embassy_net::new(wifi_interface, net_config, resources, seed);

    spawner
        .spawn(wifi::wifi_connection_task(
            controller,
            stack,
            scan_interval_attempts,
        ))
        .unwrap();
    spawner.spawn(wifi::wifi_net_task(runner)).unwrap();
    spawner.spawn(wifi::wifi_status_task(stack)).unwrap();
    spawner.spawn(mdns::mdns_task(stack)).unwrap();
    #[cfg(feature = "http-server")]
    if super::status::feature_http_enabled() {
        spawner.spawn(http::http_status_task(stack)).unwrap();
    }
    spawner.spawn(ntp::ntp_sync_task(stack)).unwrap();
    spawner.spawn(udp::udp_discovery_task(stack)).unwrap();
    spawner.spawn(udp::udp_telemetry_task(stack)).unwrap();
}

fn normalized_dhcp_hostname(input: &str) -> heapless::String<32> {
    shared::normalized_dhcp_hostname(input)
}

/// Returns `(packets_sent, packets_failed)` from the UDP telemetry sender since boot.
pub(crate) fn udp_telemetry_stats() -> (u32, u32) {
    udp::telemetry_stats()
}

/// Returns the most recently discovered server IPv4 octets, or `None` if not yet discovered.
pub(crate) fn udp_discovered_server_ip_octets() -> Option<[u8; 4]> {
    udp::discovered_server_ip_octets()
}
