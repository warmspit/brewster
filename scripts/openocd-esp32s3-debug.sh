#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
workspace_root="$(cd -- "${script_dir}/.." && pwd)"

if [[ -n "${OPENOCD_ESP32_BIN:-}" ]]; then
  openocd_bin="${OPENOCD_ESP32_BIN}"
else
  openocd_bin="$(find "${HOME}/.espressif/tools/openocd-esp32" -type f -path '*/openocd-esp32/bin/openocd' 2>/dev/null | sort | tail -n 1)"
fi

if [[ -z "${openocd_bin}" || ! -x "${openocd_bin}" ]]; then
  echo "error: Espressif OpenOCD not found under ~/.espressif/tools/openocd-esp32" >&2
  echo "Install the ESP-IDF/OpenOCD toolchain or set OPENOCD_ESP32_BIN to the binary path." >&2
  exit 1
fi

# Kill any existing OpenOCD instance so ports 3333/6666/4444 are free
pkill -x openocd 2>/dev/null || true
sleep 0.3

exec "${openocd_bin}" -f "${workspace_root}/openocd.cfg" "$@"
