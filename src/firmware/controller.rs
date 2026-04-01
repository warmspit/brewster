use embassy_time::{Duration, Timer};
use esp_hal::delay::Delay;
use esp_hal::gpio::{Flex, Level, Output};
use pid::Pid;

use super::{config, sensor, status};
use super::error::SensorError;

#[derive(Clone, Copy)]
pub struct Rgb8 {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

fn status_color(temp_c: f32, heating_on: bool) -> Rgb8 {
    let _ = temp_c;

    if heating_on {
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
    // Green = device alive, red = fault.
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

pub async fn control_step(
    delay: &mut Delay,
    one_wire_pin: &mut Flex<'static>,
    relay: &mut Output<'static>,
    pid: &mut Pid<f32>,
    window_step: &mut u32,
) -> (Rgb8, bool) {
    match sensor::ds18b20_start_conversion(one_wire_pin, delay) {
        Ok(()) => {
            Timer::after(Duration::from_millis(config::DS18B20_CONVERSION_MS)).await;

            match sensor::ds18b20_read_temperature_c(one_wire_pin, delay) {
                Ok(temp_c) => {
                    pid.setpoint = status::get_target_temp_c();
                    let pid_output = pid.next_control_output(temp_c).output.clamp(0.0, 100.0);
                    let on_steps = compute_on_steps(pid_output);
                    let heating_on = *window_step < on_steps;

                    relay.set_level(if heating_on { Level::High } else { Level::Low });
                    let color = status_color(temp_c, heating_on);
                    status::update_success(status::RuntimeSample {
                        temp_c,
                        pid_output,
                        heating_on,
                        led_red: color.red,
                        led_green: color.green,
                        led_blue: color.blue,
                        pid_window_step: *window_step as u8,
                        pid_on_steps: on_steps as u8,
                    });
                    *window_step = (*window_step + 1) % config::SSR_WINDOW_STEPS;
                    (color, heating_on)
                }
                Err(error) => {
                    pid.reset_integral_term();
                    relay.set_low();
                    *window_step = 0;
                    let color = sensor_fault_color(error);
                    status::update_error(error, color.red, color.green, color.blue);
                    (color, false)
                }
            }
        }
        Err(error) => {
            pid.reset_integral_term();
            relay.set_low();
            *window_step = 0;
            let color = sensor_fault_color(error);
            status::update_error(error, color.red, color.green, color.blue);
            (color, false)
        }
    }
}
