#!/usr/bin/env bash
# Build both the firmware and the LAN server.
# Pass --flash         to also flash the firmware after building.
# Pass --monitor       to open espflash serial monitor after flashing (or standalone).
# Pass --run-server    to start the server after building.
# Pass --release       to build firmware in release mode (optimised, smaller binary).
# Pass --no-http       to disable the embedded HTTP server feature.
# Pass --no-prometheus to disable the Prometheus /metrics endpoint.
set -euo pipefail

FLASH=0
MONITOR=0
RUN_SERVER=0
RELEASE=0
NO_HTTP=0
NO_PROMETHEUS=0
ARGS=()
for arg in "$@"; do
  case "$arg" in
    --flash)         FLASH=1 ;;
    --monitor)       MONITOR=1 ;;
    --run-server)    RUN_SERVER=1 ;;
    --release)       RELEASE=1 ;;
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

PROFILE_DIR="debug"
RELEASE_FLAG=()
if [[ $RELEASE -eq 1 ]]; then
  PROFILE_DIR="release"
  RELEASE_FLAG=(--release)
fi
FIRMWARE_BIN="$SCRIPT_DIR/target/xtensa-esp32s3-none-elf/$PROFILE_DIR/brewster"

echo "==> firmware: cargo build"
cargo build \
  --target xtensa-esp32s3-none-elf \
  -Zbuild-std=core,alloc \
  --manifest-path "$SCRIPT_DIR/Cargo.toml" \
  "${RELEASE_FLAG[@]+"${RELEASE_FLAG[@]}"}" \
  "${FEATURE_FLAGS[@]+"${FEATURE_FLAGS[@]}"}" \
  "$@"

# ── Server (host) ─────────────────────────────────────────────────────────────
echo "==> server: cargo build"
(cd "$SCRIPT_DIR/server" && cargo build --release)

echo "==> done"
echo "    firmware: $FIRMWARE_BIN"
echo "    server:   server/target/aarch64-apple-darwin/release/brewster-server"

if [[ $FLASH -eq 1 && $MONITOR -eq 1 ]]; then
  echo "==> firmware: flashing + monitor (Ctrl-R to reset, Ctrl-C to exit)"
  # --monitor keeps the USB session alive through flash → reset → boot,
  # so boot log output is not missed between two separate espflash invocations.
  exec espflash flash \
    --no-stub \
    --partition-table "$SCRIPT_DIR/partitions.csv" \
    --monitor \
    "$FIRMWARE_BIN"
elif [[ $FLASH -eq 1 ]]; then
  echo "==> firmware: flashing"
  espflash flash \
    --no-stub \
    --partition-table "$SCRIPT_DIR/partitions.csv" \
    "$FIRMWARE_BIN"
fi

if [[ $MONITOR -eq 1 ]]; then
  echo "==> firmware: monitor (Ctrl-R to reset, Ctrl-C to exit)"
  exec espflash monitor
fi

if [[ $RUN_SERVER -eq 1 ]]; then
  echo "==> server: starting"
  export WEB_DIR="${WEB_DIR:-$SCRIPT_DIR/web}"
  # Extract sensor names from config.local.toml and pass them to the server.
  # Reads all `name = "..."` lines inside [[sensors]] blocks, joins with comma.
  if [[ -z "${SENSOR_NAMES:-}" && -f "$SCRIPT_DIR/config.local.toml" ]]; then
    SENSOR_NAMES=$(awk '
      /^\[\[sensors\]\]/ { in_sensor=1; next }
      /^\[/ && !/^\[\[sensors\]\]/ { in_sensor=0 }
      in_sensor && /^name[[:space:]]*=/ {
        gsub(/.*=[[:space:]]*"/, ""); gsub(/".*/, ""); printf "%s,", $0
      }
    ' "$SCRIPT_DIR/config.local.toml" | sed 's/,$//')
    export SENSOR_NAMES
  fi
  exec "$SCRIPT_DIR/server/target/aarch64-apple-darwin/release/brewster-server"
fi
