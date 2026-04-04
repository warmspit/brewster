# Architecture

Brewster is an ESP32-S3 firmware written in Rust. It reads a DS18B20 temperature sensor, runs a PID loop to drive a solid-state relay, and serves a live monitoring dashboard over Wi-Fi. There is no operating system; the async runtime is Embassy on top of esp-hal.

---

## Repository Layout

```text
src/
  bin/main.rs              Entry point, hardware init, control loop
  firmware/
    config.rs              Compile-time constants and build-env overrides
    controller.rs          PID step, relay output, LED color logic
    error.rs               Unified error enums (SensorError, StorageError)
    metrics.rs             JSON / text / Prometheus serializers
    mod.rs                 Module declarations
    network.rs             Wi-Fi bootstrap, static socket buffers, task spawning
    sensor.rs              DS18B20 one-wire driver (bit-bang)
    shared.rs              Cross-module types (NtpSource, peer selection logic)
    status.rs              Live atomic runtime state; re-exports from storage
    storage.rs             Flash-backed persistence (target temp, history, probe name)
    network/
      http.rs              HTTP server task (single TCP socket)
      mdns.rs              mDNS announcer and responder
      ntp.rs               NTP client with multi-peer selection
      wifi.rs              Wi-Fi station mode setup
web/
  dashboard.js             Single-file JS bundle served by the device
  dashboard.ts             TypeScript source (canonical; JS generated from this)
  api.ts / api.js          Fetch helpers and API types
  charts.ts / charts.js    Canvas chart classes (Sparkline, PidChart)
  ui.ts / ui.js            DOM utility functions
build.rs                   Sensor config codegen, env injection, linker args
partitions.csv             Flash partition table
```

---

## Firmware Modules

### `main.rs` — Entry point and control loop

`main` is an `async fn` that never returns. It:

1. Allocates two heaps: 64 KB in IRAM (reclaimed from startup code) plus 36 KB in DRAM.
2. Restores the last target temperature from the `cfg` flash partition via `storage::init_persistent_target`.
3. Starts the Embassy/esp-rtos runtime on TIMG0.
4. Configures hardware peripherals: GPIO5 (DS18B20 one-wire), GPIO12 (SSR relay output), GPIO48 (WS2812 NeoPixel via RMT), and a software PID controller.
5. Spawns all network tasks (Wi-Fi, HTTP, mDNS, NTP) via `network::configure_wifi`.
6. Enters an infinite loop calling `controller::control_step` once per second.

Between control steps the loop updates the NeoPixel colour based on `status::http_led_state()` and the sensor health, and prints a console status line at a configurable interval.

### `config.rs` — Compile-time configuration

Constants and `option_env!` reads for build-time overrides. Sensor GPIO assignments and names are injected by `build.rs` into `sensors_config.rs` and included here via `include!`. PID gains (`KP`, `KI`, `KD`), SSR window width, WS2812 timing ticks, and hostname defaults all live here.

### `controller.rs` — Control step

`control_step` is called once per second. It:

1. Triggers a DS18B20 conversion, waits 750 ms, reads the scratchpad.
2. Feeds the temperature into the PID and updates the relay on/off state using a 10-step time window (`compute_on_steps`).
3. Calls `status::update_success` or `status::update_error` to publish the result atomically.
4. Returns the LED colour for the main loop to apply.

### `sensor.rs` — DS18B20 one-wire driver

Pure bit-bang one-wire implementation using `esp_hal::gpio::Flex` in open-drain mode. Sends `SKIP_ROM` + `CONVERT_T`, waits for conversion, then sends `SKIP_ROM` + `READ_SCRATCHPAD`. Validates the 8-bit CRC. Retries up to `DS18B20_READ_ATTEMPTS` times on CRC failure.

### `status.rs` — Live atomic runtime state

Holds the in-RAM, lock-free runtime snapshot: per-sensor temperature centidegrees and status codes, PID output, relay state, LED colour, IP address, NTP sync state, NTP peer table, HTTP exchange state, and the collection-enabled flag.

All fields use `AtomicI32`, `AtomicU32`, `AtomicBool`, `AtomicU8`, or a `critical_section::Mutex<RefCell<...>>` for non-atomic types. There are no locks in the hot path; network tasks read atomics directly.

`status` re-exports everything from `storage` so all existing callers use the same import path regardless of which module owns the data.

### `storage.rs` — Flash-backed persistence

Owns all data that must survive a reboot:

| Data | Storage |
| --- | --- |
| Target temperature | `cfg` partition (raw NOR flash, 9-byte record with magic + version + CRC) |
| History ring buffer | `cfg` partition beyond `HISTORY_DATA_OFFSET`, 16 bytes/record |
| Probe name | RAM only (heapless String, set from `TEMP_PROBE_NAME` env at build time) |

The flash partition is located at init time by reading the partition table from the standard ESP-IDF partition table offset. Offset and length are stored in atomics so the hot path never re-scans.

`persist_history_sample` is `pub(crate)`. The collection-enabled guard lives in `status::update_success`, not inside storage, to avoid a circular dependency.

### `metrics.rs` — Serializers

Produces three string representations of device state by reading atomics from `status` and calling `storage::history_snapshot`:

- **JSON** (`/status` endpoint) — Full state including sensors, PID, LED, system, and NTP detail.
- **Text** — Compact human-readable summary for console debugging.
- **Prometheus** (`/metrics` endpoint) — Labelled gauge metrics for scraping.

### `error.rs` — Error types

`SensorError` (BusStuckLow, NoDevice, CrcMismatch) and `StorageError` (NotInitialized, MissingPartition, PartitionTooSmall, OutOfRange, Flash). `FirmwareError` is a unified enum wrapping both; it is not yet used in every call site.

### `shared.rs` — Cross-module utilities

Types and pure functions that would otherwise create circular imports: `NtpSource`, `NtpSelectionSample`, `should_replace_master` (NTP peer ranking logic), IPv4 parsing, ISO 8601 formatting, and the NTP peer configuration parser.

---

## Network Subsystem

### `network.rs` — Bootstrap and buffers

`configure_wifi` is called once from `main`. It reads `SSID` and `PASSWORD` from the build environment, initialises the radio, configures DHCP with the device hostname, and spawns four Embassy tasks:

| Task | Function |
| --- | --- |
| `wifi_task` | Runs the esp-radio Wi-Fi state machine |
| `net_task` | Runs the embassy-net IP stack |
| `http_status_task` | Serves the HTTP dashboard and API |
| `mdns_task` | Announces and responds to mDNS queries |
| `ntp_task` | Syncs time |

Static socket buffers (`HTTP_RX_BUFFER`, `HTTP_TX_BUFFER`, each 1024 bytes) are allocated here using `ConstStaticCell` so they live for `'static` without heap allocation.

### `network/http.rs` — HTTP server

A single TCP socket on port 80 handles one connection at a time, in a `loop`:

1. Wait for `stack.wait_config_up()`.
2. `socket.accept(80)`.
3. `socket.read_with(|buf| parse_request(buf))` — parses the request in-place.
4. Build the response body (either a `&'static str` or a heap-allocated `String`).
5. Write the response header then the body via `socket_write_all`.
6. Flush and close.

File assets (`dashboard.js`) are embedded at compile time via `include_str!` so they are stored in flash alongside the firmware.

**Endpoints:**

| Method + Path | Response |
| --- | --- |
| `GET /` or `/panel` | HTML dashboard |
| `GET /dashboard.js` | Bundled JavaScript |
| `GET /status` | JSON device state |
| `GET /history?points=N` | JSON history ring buffer (up to N entries) |
| `GET /metrics` | Prometheus text format |
| `POST /temperature` | Set target temperature (JSON `{"temperature_c": N}`) |
| `POST /collection/start` | Enable history collection |
| `POST /collection/stop` | Disable history collection |
| `POST /history/clear` | Clear history ring buffer |
| `POST /probe-name` | Set temperature probe display name |

Because responses can be tens of kilobytes (the JS bundle is ~41 KB), the HTTP task streams the body in chunks via `socket_write_all` rather than buffering the entire response.

### `network/mdns.rs` — mDNS

A UDP socket on port 5353 (joined to `224.0.0.251` and `ff02::fb`). Announces `A`/`AAAA` records for `<hostname>.local` and `PTR`/`SRV`/`TXT` records for `_http._tcp.local`. Responds to queries for the same names. Announcements are sent on a timer and after IP address changes.

### `network/ntp.rs` — NTP client

Queries up to four configured NTP servers plus the DHCP gateway. Selects the best peer by stratum, then latency, then jitter (`shared::should_replace_master`). Syncs once per hour; retries every 60 seconds on failure. Uses a 32-bit nonce to detect duplicate or replayed packets.

The synced Unix timestamp and selected peer metadata are stored as atomics in `status`.

---

## Flash Memory Layout

```text
0x000000  Boot loader (Espressif ROM)
0x009000  nvs       (24 KB)   NVS key-value store (unused by firmware directly)
0x00f000  otadata   (8 KB)    OTA state
0x011000  phy_init  (4 KB)    Radio calibration
0x020000  factory   (2.875 MB) Firmware image
0x300000  cfg       (512 KB)  Target temperature record + history ring buffer
```

The `cfg` partition is accessed directly via `esp-storage` (raw NOR flash API). The first 4 KB sector holds the 9-byte target temperature record. Starting at `HISTORY_DATA_OFFSET` (0x1000 into the partition), 16-byte history records are written sequentially in a ring buffer. The capacity is calculated from the partition size at init time.

---

## Frontend

`dashboard.js` is a single self-contained JavaScript file (~41 KB) with no runtime dependencies and no module imports. It is embedded in the firmware binary at build time via `include_str!` and served verbatim from the HTTP task.

The source is split across four TypeScript files for maintainability; a Python bundler concatenates them in dependency order and strips TypeScript syntax: `ui.ts` → `api.ts` → `charts.ts` → `dashboard.ts`.

**Source module responsibilities:**

| File | Contents |
| --- | --- |
| `ui.ts` | DOM helpers (`byId`, `setText`), display formatters (`formatTemp`, `formatUptime`) |
| `api.ts` | Fetch wrappers for all HTTP endpoints; API payload types |
| `charts.ts` | `Sparkline` and `PidChart` canvas chart classes; zoom/pan state |
| `dashboard.ts` | Application entry point: polling loop, state, event bindings |

**Data flow in the browser:**

1. `start()` runs on page load. It finds the chart canvas elements and constructs `Sparkline` and `PidChart` instances.
2. `loadHistoryFromDevice` fetches `/history` and populates the charts with historical data.
3. `loop()` fires every 5 seconds via `setInterval`. It fetches `/status`, calls `updateFromStatus` to refresh all KPI elements, and (while collecting) calls `mergeHistoryFromDevice` to append new history points.
4. User actions (set target, start/stop collection, clear history, rename probe) call the corresponding `api.ts` fetch wrappers.

**Chart zoom/pan** is driven by wheel and double-click events on the canvas. `zoomStart` and `zoomEnd` (floats 0–1) define the visible window as a fraction of the full dataset. All charts share the same window and redraw in sync.

---

## State Ownership Summary

```text
                    ┌─────────────┐
                    │   main.rs   │  hardware init, control loop
                    └──────┬──────┘
                           │ writes every 1 s
                           ▼
                    ┌─────────────┐      ┌─────────────┐
                    │  status.rs  │◄─────│  storage.rs │
                    │  (atomics)  │      │  (flash/RAM) │
                    └──────┬──────┘      └─────────────┘
                           │ reads
              ┌────────────┼────────────┐
              ▼            ▼            ▼
         ┌─────────┐ ┌──────────┐ ┌─────────┐
         │ http.rs │ │ mdns.rs  │ │  ntp.rs │
         │ (reads  │ │ (reads   │ │ (writes │
         │  + HTTP │ │  IP for  │ │  NTP    │
         │  writes │ │  records)│ │  state) │
         │  target)│ └──────────┘ └─────────┘
         └────┬────┘
              │ include_str!
              ▼
       dashboard.js (in flash)
              │ served over HTTP
              ▼
          Browser
```

`status.rs` is the single source of truth for live runtime state. `storage.rs` is the single owner of flash I/O. `metrics.rs` reads from both and produces serialized output. Network tasks read state but do not share locks with the control loop — they only touch atomics or use `critical_section` Mutex guards for the short-lived NTP peer table update.
