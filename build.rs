// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

fn main() {
    inject_local_wifi_env();
    generate_sensor_config();
    linker_be_nice();
    // make sure linkall.x is the last linker script (otherwise might cause problems with flip-link)
    println!("cargo:rustc-link-arg=-Tlinkall.x");
}

fn generate_sensor_config() {
    let local_cfg = "config.local.toml";
    const MAX_SENSORS: usize = 3;
    println!("cargo:rerun-if-changed={local_cfg}");

    let contents = std::fs::read_to_string(local_cfg).unwrap_or_default();
    let (mut sensors, mut control_index) = extract_sensor_blocks(&contents);

    if sensors.is_empty() {
        sensors.push((5, "probe-1".to_string()));
    }

    if sensors.len() > MAX_SENSORS {
        println!(
            "cargo:warning=Only the first 3 sensors are supported; truncating config.local.toml [[sensors]] list"
        );
        sensors.truncate(MAX_SENSORS);
    }

    // Keep control index valid even after truncation.
    if control_index >= sensors.len() {
        control_index = 0;
    }

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let output = std::path::Path::new(&out_dir).join("sensors_config.rs");

    let mut generated = String::from("pub const SENSORS: &[SensorConfig] = &[\n");
    for (pin, name) in sensors {
        generated.push_str("    SensorConfig { pin: ");
        generated.push_str(&pin.to_string());
        generated.push_str(", name: ");
        generated.push_str(&format!("{:?}", name));
        generated.push_str(" },\n");
    }
    generated.push_str("];\n");

    // Add control probe index (default to 0 if not specified)
    generated.push_str("\n#[allow(dead_code)]\npub const CONTROL_PROBE_INDEX: usize = ");
    generated.push_str(&control_index.to_string());
    generated.push_str(";\n");

    std::fs::write(output, generated).expect("failed to write generated sensors config");
}

fn extract_sensor_blocks(contents: &str) -> (Vec<(u8, String)>, usize) {
    let mut sensors: Vec<(u8, String)> = Vec::new();
    let mut control_index: usize = 0;
    let mut control_found = false;
    let mut current_pin: Option<u8> = None;
    let mut current_name: Option<String> = None;
    let mut current_control: bool = false;
    let mut in_sensor_block = false;
    let mut sensor_count = 0;

    let mut finalize_current = |pin: &mut Option<u8>,
                                name: &mut Option<String>,
                                is_control: &mut bool,
                                index: &mut usize,
                                control_index: &mut usize,
                                control_found: &mut bool| {
        if let (Some(p), Some(n)) = (pin.take(), name.take()) {
            sensors.push((p, n));
            if *is_control && !*control_found {
                *control_index = *index;
                *control_found = true;
            }
            *index += 1;
            *is_control = false;
        }
    };

    for line in contents.lines() {
        let without_comment = line.split('#').next().unwrap_or("");
        let trimmed = without_comment.trim();
        if trimmed.is_empty() {
            continue;
        }

        if trimmed == "[[sensors]]" {
            if in_sensor_block {
                finalize_current(
                    &mut current_pin,
                    &mut current_name,
                    &mut current_control,
                    &mut sensor_count,
                    &mut control_index,
                    &mut control_found,
                );
            }
            in_sensor_block = true;
            current_pin = None;
            current_name = None;
            current_control = false;
            continue;
        }

        if trimmed.starts_with('[') {
            if in_sensor_block {
                finalize_current(
                    &mut current_pin,
                    &mut current_name,
                    &mut current_control,
                    &mut sensor_count,
                    &mut control_index,
                    &mut control_found,
                );
                in_sensor_block = false;
                current_pin = None;
                current_name = None;
                current_control = false;
            }
            continue;
        }

        if !in_sensor_block {
            continue;
        }

        let Some((lhs, rhs)) = trimmed.split_once('=') else {
            continue;
        };
        let key = lhs.trim();
        let value = rhs.trim();

        match key {
            "pin" => {
                let parsed = value.trim_matches('"').parse::<u8>().ok();
                current_pin = parsed;
            }
            "name" => {
                let mut name = value.to_string();
                if name.starts_with('"') && name.ends_with('"') && name.len() >= 2 {
                    name = name[1..name.len() - 1].to_string();
                }
                current_name = Some(name);
            }
            "control" => {
                current_control = value.eq_ignore_ascii_case("true");
            }
            _ => {}
        }
    }

    if in_sensor_block {
        finalize_current(
            &mut current_pin,
            &mut current_name,
            &mut current_control,
            &mut sensor_count,
            &mut control_index,
            &mut control_found,
        );
    }

    (sensors, control_index)
}

fn inject_local_wifi_env() {
    let local_cfg = "config.local.toml";
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
        "UDP_SERVER_IP",
        "UDP_SERVER_PORT",
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
