// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

fn main() {
    inject_local_wifi_env();
    linker_be_nice();
    // make sure linkall.x is the last linker script (otherwise might cause problems with flip-link)
    println!("cargo:rustc-link-arg=-Tlinkall.x");
}

fn inject_local_wifi_env() {
    let local_cfg = ".cargo/config.local.toml";
    println!("cargo:rerun-if-changed={local_cfg}");

    let Ok(contents) = std::fs::read_to_string(local_cfg) else {
        return;
    };

    for key in [
        "SSID",
        "PASSWORD",
        "DEVICE_HOSTNAME",
        "TEMP_PROBE_NAME",
        "WIFI_SCAN_EVERY_ATTEMPTS",
        "STATUS_PRINT_EVERY_SECONDS",
        "NTP_SERVERS",
        "NTP_SERVER",
    ] {
        if let Some(value) = extract_env_value(&contents, key) {
            println!("cargo:rustc-env={key}={value}");
        }
    }
}

fn extract_env_value(contents: &str, key: &str) -> Option<String> {
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let Some((lhs, rhs)) = trimmed.split_once('=') else {
            continue;
        };

        if lhs.trim() != key {
            continue;
        }

        let mut value = rhs.trim().to_string();
        if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
            value = value[1..value.len() - 1].to_string();
        }

        return Some(value);
    }

    None
}

fn linker_be_nice() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let kind = &args[1];
        let what = &args[2];

        match kind.as_str() {
            "undefined-symbol" => match what.as_str() {
                what if what.starts_with("_defmt_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `defmt` not found - make sure `defmt.x` is added as a linker script and you have included `use defmt_rtt as _;`"
                    );
                    eprintln!();
                }
                "_stack_start" => {
                    eprintln!();
                    eprintln!("💡 Is the linker script `linkall.x` missing?");
                    eprintln!();
                }
                what if what.starts_with("esp_rtos_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `esp-radio` has no scheduler enabled. Make sure you have initialized `esp-rtos` or provided an external scheduler."
                    );
                    eprintln!();
                }
                "free"
                | "malloc"
                | "calloc"
                | "get_free_internal_heap_size"
                | "malloc_internal"
                | "realloc_internal"
                | "calloc_internal"
                | "free_internal" => {
                    eprintln!();
                    eprintln!(
                        "💡 Did you forget the `esp-alloc` dependency or didn't enable the `compat` feature on it?"
                    );
                    eprintln!();
                }
                _ => (),
            },
            // we don't have anything helpful for "missing-lib" yet
            _ => {
                std::process::exit(1);
            }
        }

        std::process::exit(0);
    }

    println!(
        "cargo:rustc-link-arg=-Wl,--error-handling-script={}",
        std::env::current_exe().unwrap().display()
    );
}
