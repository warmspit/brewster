// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

extern crate alloc;

#[path = "../firmware/mod.rs"]
mod firmware;

use firmware::{config, controller, metrics, network, sensor, status};

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_alloc as _;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Flex, Level, Output, OutputConfig};
use esp_hal::ram;
use esp_hal::rmt::{PulseCode, Rmt, TxChannelConfig, TxChannelCreator};
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use esp_println::println;
use pid::Pid;

pub use firmware::config::{PID_KD, PID_KI, PID_KP};
pub use firmware::error::SensorError;

fn status_print_every_seconds() -> u64 {
    config::status_print_every_seconds()
}

pub fn device_hostname() -> &'static str {
    config::device_hostname()
}

pub fn temp_probe_name() -> &'static str {
    config::temp_probe_name()
}

fn status_print_interval_cycles() -> u32 {
    config::status_print_interval_cycles()
}

fn pixel_frame(color: controller::Rgb8) -> [PulseCode; 25] {
    let mut frame = [PulseCode::end_marker(); 25];
    let bytes = [color.green, color.red, color.blue];
    let mut index = 0;

    for byte in bytes {
        for bit in (0..8).rev() {
            let is_one = (byte & (1 << bit)) != 0;
            frame[index] = if is_one {
                PulseCode::new(
                    Level::High,
                    config::WS2812_T1H_TICKS,
                    Level::Low,
                    config::WS2812_T1L_TICKS,
                )
            } else {
                PulseCode::new(
                    Level::High,
                    config::WS2812_T0H_TICKS,
                    Level::Low,
                    config::WS2812_T0L_TICKS,
                )
            };
            index += 1;
        }
    }

    frame[24] = PulseCode::end_marker();
    frame
}

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // generator version: 1.2.0

    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 64 * 1024);
    esp_alloc::heap_allocator!(size: 36 * 1024);

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    if let Err(error) = status::set_temp_probe_name(temp_probe_name()) {
        println!(
            "probe name config invalid ({:?}), using default {}",
            error,
            status::temp_probe_name()
        );
    }

    let restored_target = status::init_persistent_target(peripherals.FLASH);
    if let Some(target_c) = restored_target {
        println!(
            "target restored from flash: {:.2}C/{:.2}F",
            target_c,
            target_c * 9.0 / 5.0 + 32.0
        );
    } else {
        println!(
            "target using default: {:.2}C/{:.2}F",
            status::get_target_temp_c(),
            status::get_target_temp_c() * 9.0 / 5.0 + 32.0
        );
    }

    // Restore collection state from flash so a power cycle resumes collection.
    if status::collection_enabled_persisted() {
        status::set_collection_enabled(true);
        println!("collection: resumed from flash (was active before reboot)");
    } else {
        println!("collection: starting idle (not active before reboot)");
    }

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0);

    let mut delay = Delay::new();

    // Give the USB Serial/JTAG host a moment to re-enumerate after reset so
    // the first boot logs are not lost.
    Timer::after(Duration::from_millis(300)).await;

    println!("{} booting", device_hostname());
    println!(
        "target={:.1}C probe={} pid={{kp:{:.2}, ki:{:.2}, kd:{:.2}}} pins={{ds18b20:GPIO5, ssr-cool:GPIO12, ssr-heat:GPIO13, led:GPIO48}}",
        status::get_target_temp_c(),
        status::temp_probe_name(),
        PID_KP,
        PID_KI,
        PID_KD,
    );

    let one_wire_pin = sensor::configure_one_wire_pin(Flex::new(peripherals.GPIO5));
    let mut one_wire_pin = one_wire_pin;
    match sensor::ds18b20_configure_resolution(
        &mut one_wire_pin,
        &mut delay,
        config::ds18b20_resolution_bits(),
    ) {
        Ok(()) => println!(
            "ds18b20: resolution set to {} bits ({} ms conversion)",
            config::ds18b20_resolution_bits(),
            config::ds18b20_conversion_ms()
        ),
        Err(e) => println!("ds18b20: resolution config failed: {:?} — using hardware default", e),
    }

    let mut relay = Output::new(peripherals.GPIO12, Level::Low, OutputConfig::default());
    let mut heat_relay = Output::new(peripherals.GPIO13, Level::Low, OutputConfig::default());

    let rmt = Rmt::new(peripherals.RMT, Rate::from_mhz(80)).unwrap();
    let led_config = TxChannelConfig::default()
        .with_clk_divider(8)
        .with_idle_output(true)
        .with_idle_output_level(Level::Low);
    let mut neopixel = rmt
        .channel0
        .configure_tx(peripherals.GPIO48, led_config)
        .unwrap();

    let mut pid = Pid::new(
        status::get_target_temp_c(),
        config::PID_OUTPUT_LIMIT_PERCENT,
    );
    pid.p(PID_KP, config::PID_OUTPUT_LIMIT_PERCENT)
        .i(PID_KI, config::PID_OUTPUT_LIMIT_PERCENT)
        .d(PID_KD, config::PID_OUTPUT_LIMIT_PERCENT);

    let mut window_step = 0u32;
    let startup_frame = pixel_frame(controller::Rgb8 {
        red: 0,
        green: 0,
        blue: 10,
    });
    neopixel = neopixel.transmit(&startup_frame).unwrap().wait().unwrap();
    println!("startup LED frame sent");

    network::configure_wifi(&spawner, peripherals.WIFI, device_hostname());

    let boot_ok_frame = pixel_frame(controller::Rgb8 {
        red: 0,
        green: 4,
        blue: 0,
    });
    neopixel = neopixel.transmit(&boot_ok_frame).unwrap().wait().unwrap();
    status::update_led(0, 4, 0);
    println!("boot complete LED frame sent");
    Timer::after(Duration::from_millis(config::BOOT_OK_DISPLAY_MS)).await;
    let print_every_seconds = status_print_every_seconds();
    let print_every_cycles = status_print_interval_cycles();
    println!(
        "status: console print interval set to every {} second(s)",
        print_every_seconds
    );
    let mut status_print_cycle = 0u32;

    loop {
        let (_color, _heating_on) = controller::control_step(
            &mut delay,
            &mut one_wire_pin,
            &mut relay,
            &mut heat_relay,
            &mut pid,
            &mut window_step,
        )
        .await;

        // Priority: HTTP error (red) > HTTP ok (blue) > UDP send (violet) > idle.
        let display_color = match status::http_led_state() {
            status::HttpLedState::Idle => {
                if status::runtime_error_active() {
                    controller::Rgb8 {
                        red: 10,
                        green: 0,
                        blue: 0,
                    }
                } else if status::udp_led_active() {
                    controller::Rgb8 {
                        red: 6,
                        green: 0,
                        blue: 10,
                    }
                } else {
                    controller::Rgb8 {
                        red: 0,
                        green: 4,
                        blue: 0,
                    }
                }
            }
            status::HttpLedState::ActiveOk => controller::Rgb8 {
                red: 0,
                green: 0,
                blue: 10,
            },
            status::HttpLedState::ActiveError => controller::Rgb8 {
                red: 10,
                green: 0,
                blue: 0,
            },
        };

        let frame = pixel_frame(display_color);
        neopixel = neopixel.transmit(&frame).unwrap().wait().unwrap();
        status_print_cycle += 1;
        if status_print_cycle >= print_every_cycles {
            status_print_cycle = 0;
            println!("{}", metrics::text());
        }

        Timer::after(Duration::from_millis(
            config::CONTROL_PERIOD_MS.saturating_sub(config::ds18b20_conversion_ms()),
        ))
        .await;
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.0.0/examples
}
