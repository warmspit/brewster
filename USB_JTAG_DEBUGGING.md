# ESP32-S3 USB JTAG Debugging

This repository is set up to debug an ESP32-S3 over the built-in USB JTAG interface using:

- Espressif OpenOCD from `~/.espressif/tools/openocd-esp32/...`
- `espflash` for flashing
- VS Code `cppdbg` for the GDB client

The Homebrew `openocd` binary is not the one to use here.

## Working Flow

Press `F5` and choose `Debug ESP32-S3 (USB JTAG)`.

That launch configuration runs the `debug-server` task, which does this in order:

1. Builds the firmware
2. Flashes the ELF with `espflash`
3. Starts Espressif OpenOCD with the ESP32-S3 built-in USB JTAG board config
4. Waits until OpenOCD is actually listening on GDB port `3333`
5. Attaches GDB from VS Code

Firmware `println!` output does not go to the VS Code Debug Console. It is emitted on the ESP32-S3 USB Serial/JTAG serial interface, so use the `monitor (serial)` task if you want to see logs while debugging.

If the firmware is already flashed and you only want to start the debugger server, use `Attach ESP32-S3 (USB JTAG)`.

## Key Files

- `.vscode/launch.json`: VS Code debug configurations
- `.vscode/tasks.json`: build, flash, and debug-server tasks
- `openocd.cfg`: repo-local OpenOCD config that sources `board/esp32s3-builtin.cfg`
- `scripts/openocd-esp32s3-debug.sh`: wrapper that auto-discovers the installed Espressif OpenOCD binary

## Manual Commands

Start only OpenOCD:

```bash
./scripts/openocd-esp32s3-debug.sh
```

You should see:

```text
Info : [esp32s3] starting gdb server on 3333
Info : Listening on port 3333 for gdb connections
```

Flash manually:

```bash
. ~/export-esp.sh && espflash flash --chip esp32s3 --partition-table partitions.csv target/xtensa-esp32s3-none-elf/debug/brewster
```

Start a passive serial monitor for `println!` output:

```bash
stty -f /dev/cu.usbmodem2101 115200 raw -echo -crtscts && cat /dev/cu.usbmodem2101
```

## Verified Result

The current setup has been validated against this board:

- `espflash` detects and flashes `/dev/cu.usbmodem2101`
- Espressif OpenOCD detects the on-chip USB JTAG adapter
- OpenOCD starts the GDB server on `localhost:3333`
- `xtensa-esp32s3-elf-gdb` can attach successfully

## Troubleshooting

- If `localhost:3333` is busy, stop the existing OpenOCD process before starting another debug session.
- If VS Code says it cannot connect, make sure the task finished with `Listening on port 3333 for gdb connections`.
- If the wrapper script cannot find OpenOCD, install the Espressif toolchain or set `OPENOCD_ESP32_BIN` to the OpenOCD binary path.
- The warning `Adapter driver already configured, ignoring` is harmless with the current board config.
- To catch the very first boot logs, start `monitor (serial)` before pressing `F5`.
- If you press Pause while the firmware is blocked in `Timer::after(...)` or otherwise waiting on interrupts, GDB will usually stop in the RTOS idle hook at `waiti 0`. That is expected. Use `Debug ESP32-S3 (USB JTAG)` to reset and break in [src/bin/main.rs](src/bin/main.rs#L221), or set breakpoints in your own code before continuing.
