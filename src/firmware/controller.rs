// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

use embassy_time::{Duration, Timer};
use esp_hal::delay::Delay;
use esp_hal::gpio::{Flex, Level, Output};
use pid::Pid;

use super::error::SensorError;
use super::{config, sensor, status};

#[derive(Clone, Copy)]
pub struct Rgb8 {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

fn status_color(relay_on: bool) -> Rgb8 {
    if relay_on {
        return Rgb8 {
            red: 10,
            green: 4,
            blue: 0,
        };
    }

    // Device is running: green indicates healthy loop execution.
    Rgb8 {
        red: 0,
        green: 4,
        blue: 0,
    }
}

fn sensor_fault_color(_error: SensorError) -> Rgb8 {
    // Amber: device is alive but sensor has faulted.
    Rgb8 {
        red: 10,
        green: 4,
        blue: 0,
    }
}

pub fn compute_on_steps(pid_output: f32) -> u32 {
    let scaled_steps = (pid_output / 100.0) * config::SSR_WINDOW_STEPS as f32;
    if scaled_steps <= 0.0 {
        0
    } else if scaled_steps >= config::SSR_WINDOW_STEPS as f32 {
        config::SSR_WINDOW_STEPS
    } else {
        (scaled_steps + 0.5) as u32
    }
}

fn on_sensor_error(
    pid: &mut Pid<f32>,
    heat_pid: &mut Pid<f32>,
    relay: &mut Output<'static>,
    heat_relay: &mut Output<'static>,
    window_step: &mut u32,
    heat_window_step: &mut u32,
    error: SensorError,
) -> (Rgb8, bool) {
    pid.reset_integral_term();
    heat_pid.reset_integral_term();
    relay.set_low();
    heat_relay.set_low();
    *window_step = 0;
    *heat_window_step = 0;
    let color = sensor_fault_color(error);
    status::update_error(error, color.red, color.green, color.blue);
    (color, false)
}

pub async fn control_step(
    delay: &mut Delay,
    one_wire_pin: &mut Flex<'static>,
    relay: &mut Output<'static>,
    heat_relay: &mut Output<'static>,
    pid: &mut Pid<f32>,
    heat_pid: &mut Pid<f32>,
    window_step: &mut u32,
    heat_window_step: &mut u32,
    last_target_c: &mut f32,
) -> (Rgb8, bool) {
    match sensor::ds18b20_start_conversion(one_wire_pin, delay) {
        Ok(()) => {
            Timer::after(Duration::from_millis(config::ds18b20_conversion_ms())).await;

            match sensor::ds18b20_read_temperature_c(one_wire_pin, delay) {
                Ok(temp_c) => {
                    if !status::collection_enabled() {
                        pid.reset_integral_term();
                        heat_pid.reset_integral_term();
                        relay.set_low();
                        heat_relay.set_low();
                        *window_step = 0;
                        *heat_window_step = 0;
                        let color = status_color(false);
                        status::update_success(status::RuntimeSample {
                            temp_c,
                            pid_output: 0.0,
                            heating_on: false,
                            heat_on: false,
                            led_red: color.red,
                            led_green: color.green,
                            led_blue: color.blue,
                            pid_window_step: 0,
                            pid_on_steps: 0,
                            pid_p_pct: 0.0,
                            pid_i_pct: 0.0,
                            pid_d_pct: 0.0,
                        });
                        return (color, false);
                    }

                    let target_c = status::get_target_temp_c();

                    // ── Setpoint change detection ─────────────────────────────
                    // When the target changes, reset the PID state and the window
                    // counter so there is no carry-over from the previous goal.
                    if (target_c - *last_target_c).abs() > 0.05 {
                        pid.reset_integral_term();
                        heat_pid.reset_integral_term();
                        *window_step = 0;
                        *heat_window_step = 0;
                        relay.set_low();
                        heat_relay.set_low();
                        *last_target_c = target_c;
                    }

                    // ── Cooling (PID) ─────────────────────────────────────────
                    // Positive output when temperature is above target.
                    //
                    // Anti-windup: this is a unidirectional (cooling-only) PID.
                    // When temp is already below target the integral has no useful
                    // work to do; if it has wound up positively from an earlier
                    // cooling run it will suppress heating for minutes.  Clear it
                    // the moment we cross below target so cooling turns off promptly.
                    //
                    // Deadband: within ±(deadband/2) of the setpoint neither relay
                    // activates — prevents both PIDs fighting near the setpoint.
                    let control_error = temp_c - target_c; // positive = above target
                    let half_band = config::ssr_deadband_c() / 2.0;
                    if control_error < 0.0 {
                        pid.reset_integral_term();
                    }
                    let (pid_output, cool_p, cool_i, cool_d) = if control_error > half_band {
                        pid.setpoint = -target_c;
                        let co = pid.next_control_output(-temp_c);
                        (co.output.clamp(0.0, 100.0), co.p, co.i, co.d)
                    } else {
                        pid.reset_integral_term();
                        (0.0, 0.0, 0.0, 0.0)
                    };
                    let cool_on_steps = compute_on_steps(pid_output);
                    let cooling_on = *window_step < cool_on_steps;
                    relay.set_level(if cooling_on { Level::High } else { Level::Low });

                    // ── Heating (PID window) ──────────────────────────────────
                    // Mirror of the cooling PID: positive output when temp < target.
                    // Anti-windup: reset integral when already above target.
                    if control_error > 0.0 {
                        heat_pid.reset_integral_term();
                    }
                    let (heat_pid_output, heat_p, heat_i, heat_d) = if control_error < -half_band {
                        heat_pid.setpoint = target_c;
                        let co = heat_pid.next_control_output(temp_c);
                        (co.output.clamp(0.0, 100.0), co.p, co.i, co.d)
                    } else {
                        heat_pid.reset_integral_term();
                        (0.0, 0.0, 0.0, 0.0)
                    };
                    let heat_on_steps = compute_on_steps(heat_pid_output);
                    // Never heat while the cooling relay is active.
                    let heat_on = !cooling_on && *heat_window_step < heat_on_steps;
                    heat_relay.set_level(if heat_on { Level::High } else { Level::Low });

                    // Transmit the active relay's commanded duty cycle and PID
                    // term contributions so the dashboard can diagnose the loop.
                    let (
                        active_pid_output,
                        active_window_step,
                        active_on_steps,
                        active_p,
                        active_i,
                        active_d,
                    ) = if cool_on_steps > 0 {
                        (
                            pid_output,
                            *window_step as u8,
                            cool_on_steps as u8,
                            cool_p,
                            cool_i,
                            cool_d,
                        )
                    } else if heat_on_steps > 0 {
                        (
                            heat_pid_output,
                            *heat_window_step as u8,
                            heat_on_steps as u8,
                            heat_p,
                            heat_i,
                            heat_d,
                        )
                    } else {
                        (0.0, 0u8, 0u8, 0.0, 0.0, 0.0)
                    };

                    let color = status_color(cooling_on || heat_on);
                    status::update_success(status::RuntimeSample {
                        temp_c,
                        pid_output: active_pid_output,
                        heating_on: cooling_on,
                        heat_on,
                        led_red: color.red,
                        led_green: color.green,
                        led_blue: color.blue,
                        pid_window_step: active_window_step,
                        pid_on_steps: active_on_steps,
                        pid_p_pct: active_p,
                        pid_i_pct: active_i,
                        pid_d_pct: active_d,
                    });
                    *window_step = (*window_step + 1) % config::SSR_WINDOW_STEPS;
                    *heat_window_step = (*heat_window_step + 1) % config::SSR_WINDOW_STEPS;
                    (color, cooling_on || heat_on)
                }
                Err(error) => on_sensor_error(
                    pid,
                    heat_pid,
                    relay,
                    heat_relay,
                    window_step,
                    heat_window_step,
                    error,
                ),
            }
        }
        Err(error) => on_sensor_error(
            pid,
            heat_pid,
            relay,
            heat_relay,
            window_step,
            heat_window_step,
            error,
        ),
    }
}
