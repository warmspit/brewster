# Architecture

Brewster is an ESP32-S3 firmware written in Rust. It reads a DS18B20 temperature sensor (up to 3 probes), runs two independent PID loops to drive a cool SSR and a heat SSR inside a configurable deadband, and serves a live monitoring dashboard over Wi-Fi. There is no operating system; the async runtime is Embassy on top of esp-hal.

A companion **LAN server** (`server/`) receives one-second UDP telemetry packets from the device and serves extended history (up to 60 days) and a richer dashboard to any browser on the local network.

---

## Repository Layout

```text
src/
  bin/main.rs              Entry point, hardware init, control loop
  firmware/
    config.rs              Compile-time constants and build-env overrides
    controller.rs          Dual-PID step, relay output, deadband, LED colour logic
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
      udp.rs               UDP telemetry sender + LAN server discovery
      wifi.rs              Wi-Fi station mode setup
web/
  index.html               Dashboard shell page (served by LAN server)
  dashboard.ts / .js       Application entry point: polling loop, state, event bindings
  api.ts / api.js          Fetch helpers and API types
  charts.ts / charts.js    Canvas chart classes (Sparkline, PidChart)
  ui.ts / ui.js            DOM utility functions
server/
  src/
    main.rs                Server entry point; configuration via env vars
    http.rs                Axum HTTP handlers (status, history, SSE events, forwarding)
    store.rs               In-memory ring buffer + LTTB downsampling + gap annotation
    persist.rs             JSONL persistence (append on insert, atomic rewrite)
    udp.rs                 UDP packet receiver and drop-detection
    packet.rs              57-byte telemetry packet codec (version 5)
    discovery.rs           UDP beacon sender + mDNS announcer (server side)
build.rs                   Sensor config codegen, env injection, linker args
partitions.csv             Flash partition table
```

---

## Firmware Modules

### `main.rs` — Entry point and control loop

`main` is an `async fn` that never returns. It:

1. Allocates two heaps: 64 KB in IRAM (reclaimed from startup code) plus 36 KB in DRAM.
2. Restores the last target temperature from the `cfg` flash partition via `storage::init_persistent_target`.
3. Restores the collection-enabled flag from flash so a power cycle resumes collection automatically.
4. Starts the Embassy/esp-rtos runtime on TIMG0.
5. Configures hardware peripherals: GPIO5 (DS18B20 one-wire), two SSR outputs (configurable pins via `build.rs`), GPIO48 (WS2812 NeoPixel via RMT), and two PID controllers (cool and heat).
6. Spawns all network tasks (Wi-Fi, HTTP, mDNS, NTP, UDP discovery, UDP telemetry) via `network::configure_wifi`.
7. Enters an infinite loop calling `controller::control_step` once per control period.

Between control steps the loop updates the NeoPixel colour based on `status::http_led_state()`, `status::udp_led_active()`, and sensor health, and prints a console status line at a configurable interval.

### `config.rs` — Compile-time configuration

Constants and `option_env!` reads for build-time overrides. Sensor GPIO assignments and names are injected by `build.rs` into `ssr_config.rs` and included via `include!`. PID gains (`KP`, `KI`, `KD`), SSR window width, deadband width, WS2812 timing ticks, DS18B20 resolution, and hostname defaults all live here.

### `controller.rs` — Control step

`control_step` drives both the cool SSR and the heat SSR inside a configurable deadband:

1. Triggers a DS18B20 conversion (configurable bit resolution), waits for conversion time, reads the scratchpad.
2. Applies a **deadband**: if the measured temperature is within ±`deadband_c/2` of the setpoint, both relays are held off and both PID integral terms are reset.
3. Cools if `temp > target + half_band`: runs the cool PID and drives the cool SSR via `compute_on_steps` (a 15-step time-proportioning window).
4. Heats if `temp < target − half_band`: runs the heat PID. The heat relay is never energised while the cool relay is active (`heat_on = !cooling_on && …`).
5. Calls `status::update_success` or `status::update_error` to publish the result atomically.
6. Returns the LED colour for the main loop to apply.

### `sensor.rs` — DS18B20 one-wire driver

Pure bit-bang one-wire implementation using `esp_hal::gpio::Flex` in open-drain mode. Sends `SKIP_ROM` + `CONVERT_T`, waits for conversion, then sends `SKIP_ROM` + `READ_SCRATCHPAD`. Validates the 8-bit CRC. Retries up to `DS18B20_READ_ATTEMPTS` times on CRC failure.

### `status.rs` — Live atomic runtime state

Holds the in-RAM, lock-free runtime snapshot: per-sensor temperature centidegrees and status codes, PID output, relay state (cool and heat), LED colour, IP address, NTP sync state, NTP peer table, HTTP exchange state, UDP LED blink state, and the collection-enabled flag.

All fields use `AtomicI32`, `AtomicU32`, `AtomicBool`, `AtomicU8`, or a `critical_section::Mutex<RefCell<...>>` for non-atomic types. There are no locks in the hot path; network tasks read atomics directly.

`status` re-exports everything from `storage` so all existing callers use the same import path regardless of which module owns the data.

### `storage.rs` — Flash-backed persistence

Owns all data that must survive a reboot:

| Data | Storage |
| --- | --- |
| Target temperature | `cfg` partition (raw NOR flash, 9-byte record with magic + version + CRC) |
| Collection-enabled flag | `cfg` partition at `FLAGS_STORE_OFFSET_IN_PARTITION` (0x200) |
| History ring buffer | `cfg` partition beyond `HISTORY_DATA_OFFSET` (0x1000), 16 bytes/record |
| Probe name | RAM only (heapless String, set from `TEMP_PROBE_NAME` env at build time) |

The flash partition is located at init time by reading the partition table from the standard ESP-IDF partition table offset. Offset and length are stored in atomics so the hot path never re-scans. History is sub-sampled to one record per `HISTORY_SAMPLE_INTERVAL_SECS` (60 s) so the 512 KB `cfg` partition holds ~8.5 days of per-minute data.

### `metrics.rs` — Serializers

Produces three string representations of device state by reading atomics from `status` and calling `storage::history_snapshot`:

- **JSON** (`/status` endpoint) — Full state including sensors, PID, LED, system, NTP detail, and UDP telemetry stats.
- **Text** — Compact human-readable summary for console debugging.
- **Prometheus** (`/metrics` endpoint) — Labelled gauge metrics for scraping.

### `error.rs` — Error types

`SensorError` (BusStuckLow, NoDevice, CrcMismatch) and `StorageError` (NotInitialized, MissingPartition, PartitionTooSmall, OutOfRange, Flash). `FirmwareError` is a unified enum wrapping both.

### `shared.rs` — Cross-module utilities

Types and pure functions that would otherwise create circular imports: `NtpSource`, `NtpSelectionSample`, `should_replace_master` (NTP peer ranking logic), IPv4 parsing, ISO 8601 formatting, and the NTP peer configuration parser.

---

## Network Subsystem

### `network.rs` — Bootstrap and task spawning

`configure_wifi` is called once from `main`. It reads `SSID` and `PASSWORD` from the build environment, initialises the radio, configures DHCP with the device hostname, and spawns seven Embassy tasks:

| Task | Function |
| --- | --- |
| `wifi_connection_task` | Maintains the Wi-Fi association state machine |
| `wifi_net_task` | Runs the embassy-net IP stack |
| `wifi_status_task` | Updates IP address atomics when DHCP changes |
| `http_status_task` | Serves the device HTTP API |
| `mdns_task` | Announces and responds to mDNS queries |
| `ntp_sync_task` | Syncs time |
| `udp_discovery_task` | Listens for `BRWS` server beacon on port 47889 |
| `udp_telemetry_task` | Sends 57-byte telemetry packets to the discovered server |

Static socket buffers (1 KB each) are allocated with `ConstStaticCell` so they live for `'static` without heap allocation.

### `network/http.rs` — Device HTTP server

A single TCP socket on port 80 handles one connection at a time with a minimal hand-rolled HTTP/1.1 parser. File assets are **not** embedded; the dashboard is served by the LAN server instead.

**Endpoints:**

| Method + Path | Response |
| --- | --- |
| `GET /status` | JSON device state |
| `GET /history?points=N` | JSON device history ring buffer (flash-backed, per-minute samples) |
| `GET /metrics` | Prometheus text format |
| `GET /config` | JSON feature flags (http_server, prometheus) |
| `POST /temperature` | Set target temperature (JSON `{"temperature_c": N}`) |
| `POST /probe-name` | Set temperature probe display name |
| `POST /collection/start` | Enable history collection |
| `POST /collection/stop` | Disable history collection |
| `POST /history/clear` | Clear device history ring buffer |
| `POST /config` | Update feature flags |

### `network/udp.rs` — Telemetry sender and server discovery

**Discovery** (two mechanisms, first match wins):

1. **Static config** — `UDP_SERVER_IP` in `config.local.toml` (highest priority).
2. **UDP beacon** — `udp_discovery_task` listens on port 47889 for a 12-byte `BRWS` frame broadcast by `brewster-server`.
3. **mDNS** — `mdns_task` calls `udp::set_discovered_server()` when it sees a `_brewster._udp.local.` PTR record.

Once a server is discovered its IP and port are stored in `DISCOVERED_IP`/`DISCOVERED_PORT` atomics. `udp_telemetry_task` sends a 57-byte packet once per second to that address. If no server is discovered the task is silent.

The packet includes a one-shot `history_clear` flag (bit 3 of the flags byte) which tells the server to clear its own ring buffer in sync with the device.

### `network/mdns.rs` — mDNS

A UDP socket on port 5353 (joined to `224.0.0.251`). Announces `A` records for `<hostname>.local` and `PTR`/`SRV`/`TXT` records for `_http._tcp.local`. Also announces `_brewster._udp.local.` PTR records so `brewster-server` can find the device. Responds to incoming queries for the same names.

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
0x300000  cfg       (512 KB)  Target temp record + feature flags + history ring buffer
```

The `cfg` partition is accessed directly via `esp-storage` (raw NOR flash API):

| Offset in `cfg` | Contents |
| --- | --- |
| `0x000` | 9-byte target temperature record (magic `0xBE` + `0xEF`, version, f32 LE, CRC8) |
| `0x200` | Feature flags byte (http_server on/off, prometheus on/off, collection state) |
| `0x1000` | History ring buffer start (`HISTORY_DATA_OFFSET`); 16 bytes/record, 60-s cadence |

---

## LAN Server

`server/` is a standalone Rust binary (Tokio + Axum) that runs on any machine on the same local network. It provides extended history retention and a richer browser dashboard.

### Data flow

```text
  ESP32-S3 device
  ┌──────────────────────────────────────────────────┐
  │ udp_telemetry_task ──UDP 47890──► store::insert  │
  │ udp_discovery_task ◄─UDP 47889──  discovery::run │
  └──────────────────────────────────────────────────┘
                                     │
                         ┌───────────┴──────────┐
                         │   In-memory Store     │
                         │   (VecDeque, RwLock)  │
                         │   retention: 60 days  │
                         └───────────┬──────────┘
                                     │ persist::append (per insert)
                                     ▼
                          brewster-data.json (JSONL)
                                     │
                              HTTP port 8080
                                     │
                    ┌────────────────┼────────────────┐
                    ▼                ▼                 ▼
              GET /status    GET /history      GET /events (SSE)
              GET /stats     POST /temperature  …
                                     │
                               Browser / dashboard
```

### `store.rs` — Ring buffer

`Store` is an `Arc<RwLock<Inner>>` holding a `VecDeque<Record>`. On each `insert`:

- Detects sequence-number gaps (counts as `packets_dropped`).
- Mirrors the device's `collecting` flag.
- Sub-samples: stores one record per `STORE_INTERVAL_S` (1 s) to the ring buffer when collecting.

`history_data(max_points)` downsamples the ring buffer for the history API using **LTTB** (Largest Triangle Three Buckets): for each of ~`max_points` buckets, it picks the record that forms the largest triangle area with the previously-selected point and the next bucket's centroid. This preserves every visible peak and trough regardless of how aggressively the data is downsampled.

Each returned `HistoryPoint` carries a `gap_before: bool` flag computed by inspecting the raw `received_at` timestamps of all source records consumed by that output point — a `>5 s` gap in receive times marks a real data interruption. This is accurate regardless of the downsampling ratio and replaces the fragile seq-diff threshold used in earlier versions.

### `persist.rs` — Persistence

Records are appended as JSON lines to `brewster-data.json` on every insert. On startup the file is loaded and records within the retention window are restored. After loading, the file is compacted (rewritten atomically via rename) to remove aged-out records. Writes use `OpenOptions::append` so a crash during a write cannot corrupt earlier records.

### `discovery.rs` — Server-side discovery

Runs two announcement mechanisms on a 5-second loop:

1. **UDP broadcast beacon** — 12-byte `BRWS` frame (magic + server IPv4 + telemetry port + HTTP port) sent to `255.255.255.255:47889`.
2. **mDNS gratuitous** — PTR/TXT/SRV/A records for `_brewster._udp.local.` sent to `224.0.0.251:5353`.

The device's `udp_discovery_task` and `mdns_task` listen for these and call `udp::set_discovered_server()` on a match.

### `http.rs` — Server HTTP API

Built on Axum with `tower_http` middlewares (CORS permissive, `Cache-Control: no-cache` for static assets, `no-store` for API responses). Static dashboard files (HTML, JS) are served from `WEB_DIR` (default `../web`).

| Method | Path | Description |
| --- | --- | --- |
| GET | `/status` | Latest packet as device-status JSON |
| GET | `/history?points=N` | LTTB-downsampled history (default 2000, max 10000 points) with `gap_before` and `t_s` per point |
| GET | `/stats` | Packet statistics (received / dropped / drop rate) |
| GET | `/events` | Server-Sent Events stream — fires on every received UDP packet |
| POST | `/history/clear` | Purge ring buffer and compact persistence file |
| POST | `/temperature` | Forward target temperature change to the device |
| POST | `/collection/start` | Forward collection start to the device |
| POST | `/collection/stop` | Forward collection stop to the device |

The `/events` SSE endpoint uses a `tokio::sync::broadcast` channel — `udp.rs` sends one `()` per packet and each connected SSE client receives its own `BroadcastStream`. The browser uses this to update the dashboard within milliseconds of each telemetry packet rather than waiting for the 1-second polling interval.

### Wire packet format — `packet.rs`

57 bytes, little-endian, version 5:

| Bytes | Field | Notes |
| --- | --- | --- |
| 0–3 | magic | `b"BREW"` |
| 4 | version | Bump on layout change; server drops mismatched packets |
| 5–24 | hostname | Null-padded UTF-8, max 20 chars |
| 25–28 | seq | u32 LE monotonic counter |
| 29–32 | uptime_s | u32 LE |
| 33–38 | temp_centi[0..2] | i16 LE × 3; `i16::MAX` = no reading |
| 39–40 | target_centi | i16 LE |
| 41 | output_pct | 0–100 % |
| 42 | flags | bit 0 = relay_on (cool), bit 1 = collecting, bit 2 = ntp_synced, bit 3 = history_clear, bit 4 = heat_on |
| 43 | window_step | 0–15 |
| 44 | on_steps | 0–15 |
| 45–47 | sensor_status[0..2] | 0 = ok |
| 48–51 | device_ip | Sending device IPv4 |
| 52 | sensor_count | 1–3 |
| 53 | deadband_centi | Total dead zone width in 0.01 °C steps |
| 54–56 | pid_p/i/d_pct | Active PID term contributions (i8, %) |

---

## Frontend (Browser Dashboard)

The web dashboard is served by the **LAN server** from `web/` as separate static files (`index.html`, `dashboard.js`, `api.js`, `charts.js`, `ui.js`). The device no longer embeds the dashboard.

Source is authored in TypeScript; plain JS files are generated from TypeScript (no bundler — the server serves them as individual module-less scripts concatenated by a small Python build step).

**Source module responsibilities:**

| File | Contents |
| --- | --- |
| `ui.ts` | DOM helpers (`byId`, `setText`), display formatters (`formatTemp`, `formatUptime`) |
| `api.ts` | Fetch wrappers for all HTTP endpoints; API payload types; `HISTORY_FETCH_POINTS` constant |
| `charts.ts` | `Sparkline` and `PidChart` canvas chart classes; zoom/pan state; gap-before rendering |
| `dashboard.ts` | Application entry point: polling loop, state, event bindings, SSE connection |

**Data flow in the browser:**

1. `start()` runs on page load. It finds the chart canvas elements and constructs `Sparkline` and `PidChart` instances.
2. `loadHistoryFromDevice` fetches `/history` (2000 points, LTTB-downsampled). Each point's `gap_before` flag (col 13) is read directly; no seq-diff heuristics are needed.
3. An SSE connection to `/events` is opened. Each server-sent event triggers an immediate `/status` and `/history` merge, so the dashboard updates within ~1 s of each telemetry packet.
4. A 1-second `setInterval` fallback calls the same poll function for environments where SSE is unavailable.
5. `mergeHistoryFromDevice` appends only points with seq > `lastHistorySeq`, deduplicating live updates from the SSE-triggered polls.
6. User actions (set target, start/stop collection, clear history, rename probe) call the corresponding `api.ts` fetch wrappers which POST to the server; the server forwards to the device.

**Chart zoom/pan** is driven by wheel and double-click events on the canvas. `zoomStart` and `zoomEnd` (floats 0–1) define the visible window as a fraction of the full dataset. All charts share the same zoom window and redraw in sync.

---

## State Ownership Summary

```text
                    ┌─────────────┐
                    │   main.rs   │  hardware init, dual-PID control loop
                    └──────┬──────┘
                           │ writes every control period (~1 s)
                           ▼
                    ┌─────────────┐      ┌─────────────┐
                    │  status.rs  │◄─────│  storage.rs │
                    │  (atomics)  │      │  (flash/RAM) │
                    └──────┬──────┘      └─────────────┘
                           │ reads
         ┌─────────────────┼──────────────────┐
         ▼                 ▼                  ▼
    ┌─────────┐      ┌──────────┐       ┌──────────┐
    │ http.rs │      │ mdns.rs  │       │  ntp.rs  │
    │ (device │      │ (IP for  │       │ (writes  │
    │  API)   │      │  records)│       │  NTP     │
    └─────────┘      └──────────┘       │  state)  │
                                        └──────────┘
         ▼
    ┌──────────┐
    │  udp.rs  │  sends 57-byte packets every 1 s
    └────┬─────┘
         │ UDP port 47890
         ▼
    ┌──────────────────────────────┐
    │  brewster-server (LAN host)  │
    │  udp.rs → store.rs           │
    │           persist.rs (JSONL) │
    │  http.rs → browser           │
    └──────────────────────────────┘
```

`status.rs` is the single source of truth for live device runtime state. `storage.rs` is the single owner of flash I/O. `metrics.rs` reads from both and produces serialised output. Network tasks read state but do not share locks with the control loop — they only touch atomics or use `critical_section` Mutex guards for the short-lived NTP peer table update.

On the server side, `Store` is the single source of truth. `persist.rs` is its only I/O path. HTTP handlers and the SSE broadcaster read from `Store` through shared `Arc` clones; they never touch the persistence file directly.
