#!/usr/bin/env bash
# Build both the firmware and the LAN server.
# Pass --flash         to also flash the firmware after building.
# Pass --monitor       to open espflash serial monitor after flashing (or standalone).
# Pass --run-server    to start the server after building.
# Pass --no-http       to disable the embedded HTTP server feature.
# Pass --no-prometheus to disable the Prometheus /metrics endpoint.
set -euo pipefail

FLASH=0
MONITOR=0
RUN_SERVER=0
NO_HTTP=0
NO_PROMETHEUS=0
ARGS=()
for arg in "$@"; do
  case "$arg" in
    --flash)         FLASH=1 ;;
    --monitor)       MONITOR=1 ;;
    --run-server)    RUN_SERVER=1 ;;
    --no-http)       NO_HTTP=1 ;;
    --no-prometheus) NO_PROMETHEUS=1 ;;
    *)               ARGS+=("$arg") ;;
  esac
done
set -- "${ARGS[@]+"${ARGS[@]}"}"

# Build up the --features / --no-default-features flags for the firmware.
FEATURE_FLAGS=()
if [[ $NO_HTTP -eq 1 ]]; then
  # Disabling http-server also disables prometheus (it depends on http-server).
  FEATURE_FLAGS=(--no-default-features)
elif [[ $NO_PROMETHEUS -eq 1 ]]; then
  FEATURE_FLAGS=(--no-default-features --features http-server)
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ── Firmware (xtensa-esp32s3-none-elf) ───────────────────────────────────────
echo "==> firmware: sourcing ESP toolchain"
# shellcheck source=/dev/null
. ~/export-esp.sh

echo "==> firmware: cargo build"
cargo build \
  --target xtensa-esp32s3-none-elf \
  -Zbuild-std=core,alloc \
  --manifest-path "$SCRIPT_DIR/Cargo.toml" \
  "${FEATURE_FLAGS[@]+"${FEATURE_FLAGS[@]}"}" \
  "$@"

# ── Server (host) ─────────────────────────────────────────────────────────────
echo "==> server: cargo build"
(cd "$SCRIPT_DIR/server" && cargo build --release)

echo "==> done"
echo "    firmware: target/xtensa-esp32s3-none-elf/debug/brewster"
echo "    server:   server/target/aarch64-apple-darwin/release/brewster-server"

if [[ $FLASH -eq 1 && $MONITOR -eq 1 ]]; then
  echo "==> firmware: flashing + monitor (Ctrl-R to reset, Ctrl-C to exit)"
  # --monitor keeps the USB session alive through flash → reset → boot,
  # so boot log output is not missed between two separate espflash invocations.
  exec espflash flash \
    --no-stub \
    --partition-table "$SCRIPT_DIR/partitions.csv" \
    --monitor \
    "$SCRIPT_DIR/target/xtensa-esp32s3-none-elf/debug/brewster"
elif [[ $FLASH -eq 1 ]]; then
  echo "==> firmware: flashing"
  espflash flash \
    --no-stub \
    --partition-table "$SCRIPT_DIR/partitions.csv" \
    "$SCRIPT_DIR/target/xtensa-esp32s3-none-elf/debug/brewster"
fi

if [[ $MONITOR -eq 1 ]]; then
  echo "==> firmware: monitor (Ctrl-R to reset, Ctrl-C to exit)"
  exec espflash monitor
fi

if [[ $RUN_SERVER -eq 1 ]]; then
  echo "==> server: starting"
  export WEB_DIR="${WEB_DIR:-$SCRIPT_DIR/web}"
  exec "$SCRIPT_DIR/server/target/aarch64-apple-darwin/release/brewster-server"
fi
