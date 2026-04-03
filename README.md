# Brewster

`brewster` is an ESP32-S3 Rust firmware project for a small temperature-controlled brewing setup.
It reads a DS18B20 temperature sensor, drives a solid-state relay with a PID loop, syncs time with NTP, exposes runtime status and Prometheus metrics over Wi-Fi, and persists the target temperature in flash.

## What The Firmware Does

- Samples a DS18B20 on GPIO5.
- Runs a PID controller once per second.
- Drives an SSR on GPIO12 using a 10-step time window.
- Shows basic device state on a WS2812/NeoPixel on GPIO48.
- Uses LED signaling for boot/HTTP activity and HTTP errors.
- Connects to Wi-Fi in station mode.
- Advertises `<hostname>.local` over mDNS (IPv4 and IPv6 when configured).
- Advertises an mDNS HTTP service at `_http._tcp.local`.
- Serves JSON status over HTTP on port 80.
- Exposes Prometheus metrics over HTTP on `/metrics`.
- Accepts target temperature updates over HTTP.
- Supports assigning a custom name to the DS18B20 temperature probe.
- Syncs time from configured NTP servers or the DHCP gateway.
- Stores the target temperature in the `cfg` flash partition.

## Hardware Assumptions

The current code is written around these pins:

- `GPIO5`: DS18B20 one-wire bus
- `GPIO12`: SSR output
- `GPIO48`: WS2812 / NeoPixel data

The firmware target is `xtensa-esp32s3-none-elf` and the project is configured for an ESP32-S3 board.

## Runtime Behavior

At boot the firmware:

1. Initializes the heap and ESP runtime.
2. Restores the last persisted target temperature from flash if present.
3. Starts the control loop and status LED.
4. Starts Wi-Fi, DHCP, mDNS, HTTP, and NTP tasks.

The control loop runs every second.
The DS18B20 conversion time is 750 ms, and the remaining time in the cycle is used as the idle gap before the next sample.

The PID output is converted to a 10-step relay window:

- `0%` means relay always off
- `100%` means relay always on
- intermediate values map to `0..10` relay-on steps per window

LED behavior:

- After boot completes, LED is steady green.
- During an HTTP exchange, LED switches to blue for the duration of the request/response.
- If the active HTTP exchange has an error status (non-200) or socket read/write failure, LED is red for the duration of that exchange.
- Outside HTTP exchanges, any runtime error (for example a failed DS18B20 probe read) makes LED steady red.
- When no HTTP exchange is active and no runtime error is present, LED is steady green.

## Network Features

### mDNS

The device responds to and periodically announces:

- `<hostname>.local`
- `_http._tcp.local` service discovery

Announcement and response details:

- Joins IPv4 mDNS multicast `224.0.0.251:5353`.
- Joins IPv6 mDNS multicast `ff02::fb:5353`.
- Publishes `A` records for `<hostname>.local` when IPv4 is configured.
- Publishes `AAAA` records for `<hostname>.local` when IPv6 is configured.
- Publishes `PTR`, `SRV`, and `TXT` records for the HTTP service.

Quick checks:

```sh
dns-sd -G v4v6 brewster.local
dns-sd -B _http._tcp local
dns-sd -L brewster _http._tcp local
```

The hostname comes from configuration and is normalized into a DHCP-safe format.

### Troubleshooting mDNS

If discovery fails from another machine, check these first.

On the Brewster serial console, confirm Wi-Fi and hostname state:

- `wifi: got IPv4 address ...`
- `wifi: hostname=...`

From another machine on the same LAN:

```sh
dns-sd -G v4v6 <hostname>.local
dns-sd -B _http._tcp local
dns-sd -L <hostname> _http._tcp local
```

If it still fails, common causes are:

- Device and client are on different VLANs/subnets without an mDNS reflector.
- AP/router has multicast isolation/filtering enabled.
- Client resolver cache is stale (flush cache or retry after reconnect).
- IPv6-only lookup path with no configured IPv6 address on the device.

Host lookup should still work over IPv4 when DHCPv4 is up.

### HTTP

The HTTP server listens on port `80`.

Supported routes:

- `GET /`
- `GET /panel`
- `GET /dashboard.js`
- `GET /status`
- `GET /history?points=<N>`
- `GET /metrics`
- `POST /temperature`
- `POST /probe-name`
- `POST /collection/start`
- `POST /collection/stop`
- `POST /history/clear`

`GET /` serves the built-in Grafana-style dashboard UI.

The dashboard polls `GET /status` for live data and visualizes temperature, PID output,
relay state, system health, and NTP master stats.

Dashboard interactions:

- hover either chart to show synchronized crosshair/tooltip on both charts
- mouse wheel to zoom time axis in/out around cursor position
- two-finger horizontal scroll/trackpad pan to move across the zoomed window
- double-click either chart to reset zoom to full history
- menu controls to start/stop collection and clear persisted history

`GET /status` returns the raw JSON status document used by the dashboard.

Example:

```sh
curl http://brewster.local/status
```

`GET /metrics` returns a Prometheus-compatible text exposition.

```sh
curl http://brewster.local/metrics
```

To update the target temperature:

```sh
curl -X POST http://brewster.local/temperature \
  -H 'Content-Type: application/json' \
  -d '{"temperature_c": 21.5}'
```

Accepted temperature range is `-20.0..=25.0` degrees C.

To start or stop data collection:

```sh
curl -X POST http://brewster.local/collection/start
curl -X POST http://brewster.local/collection/stop
```

To fetch or clear persisted history:

```sh
curl 'http://brewster.local/history?points=2000'
curl -X POST http://brewster.local/history/clear
```

To set a probe name:

```sh
curl -X POST http://brewster.local/probe-name \
  -H 'Content-Type: application/json' \
  -d '{"probe_name": "kettle"}'
```

Allowed characters for probe names: `A-Z`, `a-z`, `0-9`, space, `.`, `_`, and `-`.
Maximum length: `32` characters.

### Prometheus Metrics

`GET /metrics` returns a Prometheus text exposition. Metric families:

| Family | Type | Description |
| --- | --- | --- |
| `brewster_up` | gauge | Firmware heartbeat, always 1 |
| `brewster_uptime_seconds` | gauge | Device uptime |
| `brewster_heap_*` | gauge | Heap usage and per-region breakdown |
| `brewster_sensor_temperature_celsius/fahrenheit` | gauge | DS18B20 reading (`NaN` when no sample) |
| `brewster_temperature_c/f` | gauge | Aliases for dashboard compatibility |
| `brewster_pid_target_celsius/fahrenheit` | gauge | PID setpoint |
| `brewster_pid_kp/ki/kd` | gauge | PID gains |
| `brewster_pid_output_percent` | gauge | PID duty cycle |
| `brewster_pid_window_step` | gauge | Current time-window step index |
| `brewster_pid_on_steps` | gauge | Relay-on steps per window |
| `brewster_relay_on` | gauge | 1 when heater relay is active |
| `brewster_ntp_synced` | gauge | 1 when time is synchronized |
| `brewster_ntp_sync_total` | counter | Successful NTP sync events |
| `brewster_ntp_last_sync_uptime_seconds` | gauge | Uptime when the NTP anchor was last recorded |
| `brewster_ntp_master_info{source,address}` | gauge | Identity of the selected NTP master (gauge=1) |
| `brewster_ntp_master_stratum` | gauge | Stratum of selected master |
| `brewster_ntp_master_latency_seconds/ms` | gauge | RTT to selected master |
| `brewster_ntp_master_jitter_seconds/ms` | gauge | RTT jitter for selected master |
| `brewster_ntp_master_offset_seconds/ms` | gauge | Clock offset for selected master |
| `brewster_ntp_master_offset_jitter_seconds/ms` | gauge | Offset jitter for selected master |
| `brewster_ntp_master_success_total` | gauge | Success count for selected master |
| `brewster_ntp_master_fail_total` | gauge | Failure count for selected master |
| `brewster_ntp_peer_success_total{source,address,selected}` | counter | Per-peer successes |
| `brewster_ntp_peer_fail_total{source,address,selected}` | counter | Per-peer failures |
| `brewster_ntp_peer_latency_seconds{source,address,selected}` | gauge | Per-peer RTT |
| `brewster_ntp_peer_jitter_seconds{source,address,selected}` | gauge | Per-peer RTT jitter |
| `brewster_ntp_peer_offset_seconds{source,address,selected}` | gauge | Per-peer clock offset |
| `brewster_ntp_peer_offset_jitter_seconds{source,address,selected}` | gauge | Per-peer offset jitter |
| `brewster_ntp_peer_last_sync_uptime_seconds{source,address,selected}` | gauge | Uptime at last successful peer sync |

Per-peer metrics are emitted once per tracked peer. The `selected` label is `"true"` for the currently active master.

### NTP

The firmware probes NTP peers from:

- `NTP_SERVERS`, a comma-separated IPv4 list
- `NTP_SERVER`, a single fallback IPv4 value
- the DHCP gateway, if present

Configured peers are preferred over the DHCP gateway, and the sync task keeps per-peer statistics in the status payload.

## Configuration

Build-time configuration is taken from `.cargo/config.local.toml`.
That file is intentionally ignored by git and is read by `build.rs`, which injects selected values into the firmware via `cargo:rustc-env`.

Create `.cargo/config.local.toml` with values like:

```toml
[env]
SSID = "your-ssid"
PASSWORD = "your-password"
DEVICE_HOSTNAME = "brewster"
WIFI_SCAN_EVERY_ATTEMPTS = "6"
STATUS_PRINT_EVERY_SECONDS = "5"
TEMP_PROBE_NAME = "probe-1"
NTP_SERVERS = "129.6.15.28,194.58.204.20"
# Optional fallback when NTP_SERVERS is empty or invalid
NTP_SERVER = "8.8.8.8"
```

Supported keys:

- `SSID`: Wi-Fi SSID; if missing or empty, Wi-Fi is disabled
- `PASSWORD`: Wi-Fi password
- `DEVICE_HOSTNAME`: advertised hostname and DHCP hostname base
- `WIFI_SCAN_EVERY_ATTEMPTS`: how often failed connection retries trigger a Wi-Fi scan
- `STATUS_PRINT_EVERY_SECONDS`: serial console status print interval
- `TEMP_PROBE_NAME`: default DS18B20 probe name at boot
- `NTP_SERVERS`: comma-separated IPv4 NTP server list
- `NTP_SERVER`: single IPv4 fallback NTP server

## Toolchain And Build Setup

The repository expects the ESP Rust toolchain configured through `rust-toolchain.toml` and `.cargo/config.toml`.

Current project defaults:

- target: `xtensa-esp32s3-none-elf`
- linker: Xtensa GCC from the local ESP toolchain
- runner: `espflash flash --monitor --chip esp32s3 --partition-table partitions.csv`
- build-std: `core`, `alloc`

Network protocol support in this build:

- IPv4: enabled and configured with DHCPv4.
- IPv6: protocol support enabled in `embassy-net`.

Note: IPv6 mDNS (`AAAA`) is emitted only when the stack has an IPv6 address configured at runtime.

In new terminals, load the ESP environment before running cargo commands:

```sh
. ~/export-esp.sh
```

## Build, Run, And Flash

Check the firmware:

```sh
. ~/export-esp.sh && cargo check
```

Build the firmware:

```sh
. ~/export-esp.sh && cargo build
```

Flash and open the serial monitor:

```sh
. ~/export-esp.sh && cargo run
```

Because the runner is configured in `.cargo/config.toml`, `cargo run` will flash with `espflash` and attach the monitor.

## VS Code USB JTAG Debug (Known-Good)

This repository includes a working VS Code debug setup for ESP32-S3 USB JTAG.

Use `F5` and select:

- `Debug ESP32-S3 (USB JTAG)` to build, flash, start OpenOCD, and attach GDB.
- `Attach ESP32-S3 (USB JTAG)` to attach to an already running OpenOCD server.

Useful tasks:

- `debug-server` builds, flashes, and starts OpenOCD.
- `openocd` starts only OpenOCD.
- `monitor (serial)` opens a passive serial log terminal on `/dev/cu.usbmodem2101`.

Notes:

- Firmware `println!` output appears in the serial monitor task, not the VS Code Debug Console.
- If you need early boot logs, start `monitor (serial)` before launching the debugger.

## Persistence And Flash Layout

The partition table lives in `partitions.csv`.
The custom `cfg` partition is used for persistent configuration storage.

Current partition layout:

- `nvs`
- `otadata`
- `phy_init`
- `factory`
- `cfg`

The firmware currently stores the target temperature in `cfg` using a small versioned binary record.

## Repository Layout

```text
src/bin/main.rs           Entry point, control loop, hardware init
src/firmware/sensor.rs    DS18B20 one-wire implementation
src/firmware/network.rs   Wi-Fi, mDNS, HTTP, and NTP tasks
src/firmware/status.rs    Shared runtime state, JSON/text status, persistence
src/firmware/shared.rs    Shared utility functions
web/dashboard.ts          Dashboard source (TypeScript)
web/dashboard.js          Dashboard runtime script served by firmware (embedded by `include_str!`)
build.rs                  Build-time env injection and linker diagnostics
```

## Status Payload

`GET /status` returns a JSON document including:

- device hostname
- DS18B20 reading or sensor error state
- DS18B20 probe name
- PID target and output
- relay state
- LED RGB state
- IP state
- NTP sync state, selected master address/source, and offset/jitter
- per-peer NTP statistics (stratum, latency, jitter, offset, offset jitter, success/fail counts, last sync uptime)
- uptime
- heap usage

`GET /metrics` returns the same operational data in Prometheus text format. See the [Prometheus Metrics](#prometheus-metrics) section for the full metric list.

The serial console prints a compact text version of the same operational state at a configurable interval.

## Notes

- Wi-Fi is optional at boot; if `SSID` is not configured, the control loop still runs.
- The DHCP hostname is normalized to lowercase alphanumeric-and-dash form.
- The HTTP request parser is intentionally minimal and only understands the routes used by this firmware.
- `build.rs` also prints a few targeted linker hints for common ESP/Rust integration failures.

## Typical Workflow

1. Create `.cargo/config.local.toml` with Wi-Fi and hostname values.
2. Load the ESP toolchain environment with `. ~/export-esp.sh`.
3. Run `cargo check`.
4. Run `cargo run` to flash and monitor the device.
5. Query `http://<hostname>.local/status` once the device joins the network.

## License

This project is licensed under the BSD 3-Clause License.
See `LICENSE` for the full license text.

Copyright (c) 2026 David Bannister

Source files include SPDX identifiers:

- `SPDX-License-Identifier: BSD-3-Clause`
