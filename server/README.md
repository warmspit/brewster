# brewster-server

LAN companion server for the Brewster fermentation controller. It receives
one-second UDP telemetry packets from the device over the local network and
serves the same HTTP API that the embedded dashboard expects, so any browser
on the LAN can view history data stretching back days rather than the limited
window held in the device's flash.

---

## How it works

```text
│  brewster device (ESP32-S3)                              │
│                                                          │
│  udp_telemetry_task  ──UDP port 47890──►  store::insert  │
│  udp_discovery_task  ◄─UDP port 47889──  discovery::run  │
└──────────────────────────────────────────────────────────┘
                                                 │
                                         HTTP port 8080
                                                 │
                                           browser / dashboard
```

### Discovery

The server announces itself every 5 seconds via two mechanisms so the firmware
can locate it without a hardcoded IP:

1. **UDP broadcast beacon** — sent to `255.255.255.255:47889` as a 12-byte
   frame (`BRWS` magic + server IPv4 + telemetry port + HTTP port). The device
   listens on port 47889 and, once a beacon is received, starts sending
   telemetry and continues for up to 30 seconds after the last beacon before
   giving up.

2. **mDNS gratuitous announcement** — sent every 5 seconds to
   `224.0.0.251:5353` advertising `_brewster._udp.local.` with PTR, TXT, SRV,
   and A records. The device's mDNS scanner extracts the server IP and port
   from these records as a fallback.

### Telemetry packets

Each packet is 32 bytes. See [`src/packet.rs`](src/packet.rs) for the full
wire format. Key fields:

| Bytes | Field | Description |
| --- | --- | --- |
| 0–3 | magic | `b"BREW"` |
| 4–7 | seq | Monotonic counter (u32 LE); used for drop detection |
| 8–11 | uptime_s | Device uptime in seconds |
| 12–17 | temp_centi[0..2] | Probe temperatures in °C × 100 (i16 LE; `i16::MAX` = no reading) |
| 18–19 | target_centi | Target setpoint °C × 100 |
| 20 | output_pct | PID output 0–100 % |
| 21 | flags | bit 0 = relay on, bit 1 = collecting, bit 2 = NTP synced |
| 27–30 | device_ip | Sending device's IPv4 |
| 31 | sensor_count | Number of configured probes (1–3) |

### In-memory store

All received records are held in a `VecDeque` ordered by arrival time.
Records older than `RETENTION_HOURS` are pruned on each insert. Sequence
number gaps between consecutive packets are counted as dropped packets and
exposed via `GET /stats`.

### Persistence

The store is persisted to a local JSON file (default `./brewster-data.json`)
so data survives server restarts and remains accessible to browsers after
a power cycle.

* On startup the file is read and all records within the retention window are
  restored into the ring buffer.
* A background task flushes the current snapshot to disk every 30 seconds.
* On a clean shutdown (SIGINT / SIGTERM) the store is flushed immediately before
  exit so no data is lost.
* Writes are **atomic** (written to a `.tmp` sibling then renamed) so a crash
  during a flush cannot corrupt or empty the data file.
* Only an abrupt kill (SIGKILL / power loss) can lose up to 30 seconds of data;
  all older data survives intact.

The file path is controlled by the `DATA_FILE` environment variable.

### HTTP API

The server exposes the same REST surface as the embedded device so the
dashboard JavaScript needs no changes.

| Method | Path | Description |
| --- | --- | --- |
| GET | `/status` | Latest packet as a device-status JSON blob |
| GET | `/history?points=N` | Up to N most-recent data points (default 2000, max 10000); adaptively downsampled to span the full retention window |
| POST | `/history/clear` | Purge all stored records and reset counters |
| GET | `/stats` | Receiver-side packet statistics (received / dropped / drop rate) |
| POST | `/temperature` | No-op (returns 501; set temperature on the device directly) |
| POST | `/collecting/start` | No-op stub — returns `{"ok":true}` |
| POST | `/collecting/stop` | No-op stub — returns `{"ok":true}` |

All responses include `Cache-Control: no-store`. CORS is permissive.

#### `GET /stats` response

```json
{
  "packets_received": 3600,
  "packets_dropped": 2,
  "drop_rate_pct": 0.0555,
  "last_seq": 3601
}
```

`packets_dropped` counts inferred gaps — if the firmware sends seq 100 then
seq 103, the server adds 2 to `packets_dropped`. Backward jumps (device
restart) are not counted as drops.

---

## Configuration

All configuration is via environment variables. No config file is required.

| Variable | Default | Description |
| --- | --- | --- |
| `DEVICE_NAME` | `brewster` | Name of the device; sets the `device` field in `/status` JSON and the mDNS instance name (`{name}-server`) |
| `UDP_PORT` | `47890` | Port the server binds to for incoming telemetry packets |
| `HTTP_PORT` | `8080` | Port the HTTP server listens on |
| `RETENTION_HOURS` | `1440` | Hours of telemetry data to keep (60 days) |
| `WEB_DIR` | `../web` | Path to the dashboard static-asset directory |
| `DATA_FILE` | `./brewster-data.json` | Path to the persistence file (loaded on startup, flushed every 30 s) |

---

## Building

The server is a standard Rust binary that compiles for the **host machine** —
no embedded toolchain or cross-compilation is needed for native builds.
Cross-compiling from macOS to Linux or Windows requires the extra steps below.

### Prerequisites

- Rust stable toolchain: `rustup toolchain install stable`

> **Note:** `server/.cargo/config.toml` sets the default build target to
> `aarch64-apple-darwin`.  Pass `--target <triple>` explicitly when building
> for any other platform.

---

### macOS (native — Apple Silicon)

```sh
cd server
cargo build --release
# binary: target/aarch64-apple-darwin/release/brewster-server
```

### macOS (native — Intel)

```sh
cd server
rustup target add x86_64-apple-darwin
cargo build --release --target x86_64-apple-darwin
# binary: target/x86_64-apple-darwin/release/brewster-server
```

---

### Linux (native, on a Linux host)

```sh
cd server
cargo build --release --target x86_64-unknown-linux-gnu
# binary: target/x86_64-unknown-linux-gnu/release/brewster-server
```

For ARM Linux (e.g. Raspberry Pi running a 64-bit OS):

```sh
rustup target add aarch64-unknown-linux-gnu
# Install a cross-linker if one is not already present:
#   macOS: brew install messense/macos-cross-toolchains/aarch64-unknown-linux-gnu
#   Ubuntu: sudo apt install gcc-aarch64-linux-gnu
cargo build --release --target aarch64-unknown-linux-gnu
# binary: target/aarch64-unknown-linux-gnu/release/brewster-server
```

---

### Linux (cross-compiled from macOS using `cargo-zigbuild`)

[`cargo-zigbuild`](https://github.com/rust-cross/cargo-zigbuild) uses the Zig
compiler as a drop-in cross-linker, avoiding the need to install system
cross-toolchains.

```sh
# One-time setup
cargo install cargo-zigbuild
brew install zig
rustup target add x86_64-unknown-linux-gnu
rustup target add aarch64-unknown-linux-gnu

# x86-64 Linux
cd server
cargo zigbuild --release --target x86_64-unknown-linux-gnu
# binary: target/x86_64-unknown-linux-gnu/release/brewster-server

# ARM64 Linux (Raspberry Pi etc.)
cargo zigbuild --release --target aarch64-unknown-linux-gnu
# binary: target/aarch64-unknown-linux-gnu/release/brewster-server
```

---

### Windows (cross-compiled from macOS)

```sh
# One-time setup
rustup target add x86_64-pc-windows-gnu
brew install mingw-w64          # provides the Windows cross-linker

cd server
cargo build --release --target x86_64-pc-windows-gnu
# binary: target/x86_64-pc-windows-gnu/release/brewster-server.exe
```

For a native Windows build (on a Windows host with Rust for Windows installed):

```powershell
cd server
# Remove or rename .cargo\config.toml first — it hard-codes aarch64-apple-darwin.
cargo build --release --target x86_64-pc-windows-msvc
# binary: target\x86_64-pc-windows-msvc\release\brewster-server.exe
```

> The `mDNS` broadcast feature uses the `socket2` crate which works on all
> three platforms.  No Windows-specific patches are needed.

---

### Using `build.sh` (macOS/Linux, recommended for combined firmware + server)

From the repository root, `build.sh` builds both the firmware and the server
together and supports optional flash + run flags:

```sh
# Build firmware + server
./build.sh

# Build and start the server
./build.sh --run-server

# Build, flash firmware, then start the server
./build.sh --flash --run-server
```

---

### Run

```sh
# Defaults — UDP :47890, HTTP :8080, 60-day retention, web assets at ../web
cd server && cargo run --release

# Custom ports, longer retention, and explicit data file location
UDP_PORT=47890 HTTP_PORT=9090 RETENTION_HOURS=168 WEB_DIR=/var/www/brewster \
  DATA_FILE=/var/lib/brewster/data.json cargo run --release
```

On Windows (PowerShell):

```powershell
$env:UDP_PORT="47890"; $env:HTTP_PORT="8080"; $env:WEB_DIR="..\web"
.\target\x86_64-pc-windows-msvc\release\brewster-server.exe
```

### Check / lint

```sh
cd server
cargo check
cargo clippy
```

---

## Running as a service (macOS launchd example)

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>              <string>com.warmspit.brewster-server</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/brewster-server</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>HTTP_PORT</key>        <string>8080</string>
    <key>UDP_PORT</key>         <string>47890</string>
    <key>RETENTION_HOURS</key>  <string>168</string>
    <key>WEB_DIR</key>          <string>/usr/local/share/brewster/web</string>
  </dict>
  <key>RunAtLoad</key>          <true/>
  <key>KeepAlive</key>          <true/>
  <key>StandardOutPath</key>    <string>/var/log/brewster-server.log</string>
  <key>StandardErrorPath</key>  <string>/var/log/brewster-server.log</string>
</dict>
</plist>
```

Save to `~/Library/LaunchAgents/com.warmspit.brewster-server.plist` then:

```sh
launchctl load ~/Library/LaunchAgents/com.warmspit.brewster-server.plist
```

---

## Firmware side

The device firmware (in `src/firmware/network/udp.rs`) exposes its own
sender-side stats in the device's `GET /status` JSON under the `telemetry` key:

```json
"telemetry": {
  "packets_sent": 3602,
  "packets_failed": 0,
  "server_ip": "192.168.1.42"
}
```

`server_ip` is `null` when the server has not yet been discovered.
