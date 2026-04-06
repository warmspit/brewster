// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

use embedded_hal::delay::DelayNs;
use esp_hal::delay::Delay;
use esp_hal::gpio::{DriveMode, Flex, InputConfig, OutputConfig, Pull};

use super::error::SensorError;
use super::shared;

const ONEWIRE_SKIP_ROM: u8 = 0xCC;
const ONEWIRE_READ_SCRATCHPAD: u8 = 0xBE;
const ONEWIRE_WRITE_SCRATCHPAD: u8 = 0x4E;
const ONEWIRE_CONVERT_TEMP: u8 = 0x44;
const DS18B20_READ_ATTEMPTS: usize = 3;

pub fn configure_one_wire_pin(mut pin: Flex<'static>) -> Flex<'static> {
    let output_config = OutputConfig::default()
        .with_drive_mode(DriveMode::OpenDrain)
        .with_pull(Pull::Up);
    let input_config = InputConfig::default().with_pull(Pull::Up);

    pin.apply_output_config(&output_config);
    pin.apply_input_config(&input_config);
    pin.set_input_enable(true);
    pin.set_output_enable(true);
    pin.set_high();

    pin
}

fn one_wire_wait_for_high(pin: &mut Flex<'static>, delay: &mut Delay) -> Result<(), SensorError> {
    for _ in 0..125 {
        if pin.is_high() {
            return Ok(());
        }
        delay.delay_us(2);
    }

    Err(SensorError::BusStuckLow)
}

fn one_wire_reset(pin: &mut Flex<'static>, delay: &mut Delay) -> Result<(), SensorError> {
    one_wire_wait_for_high(pin, delay)?;

    pin.set_low();
    delay.delay_us(480);
    pin.set_high();
    delay.delay_us(70);

    if pin.is_high() {
        delay.delay_us(410);
        return Err(SensorError::NoDevice);
    }

    delay.delay_us(410);
    Ok(())
}

fn one_wire_write_bit(pin: &mut Flex<'static>, delay: &mut Delay, bit: bool) {
    pin.set_low();

    if bit {
        delay.delay_us(6);
        pin.set_high();
        delay.delay_us(64);
    } else {
        delay.delay_us(60);
        pin.set_high();
        delay.delay_us(10);
    }
}

fn one_wire_read_bit(pin: &mut Flex<'static>, delay: &mut Delay) -> bool {
    pin.set_low();
    delay.delay_us(6);
    pin.set_high();
    delay.delay_us(9);

    let bit = pin.is_high();
    delay.delay_us(55);
    bit
}

fn one_wire_write_byte(pin: &mut Flex<'static>, delay: &mut Delay, mut value: u8) {
    for _ in 0..8 {
        one_wire_write_bit(pin, delay, value & 0x01 != 0);
        value >>= 1;
    }
}

fn one_wire_read_bytes(pin: &mut Flex<'static>, delay: &mut Delay, output: &mut [u8]) {
    for byte in output.iter_mut() {
        let mut value = 0u8;

        for bit_index in 0..8 {
            if one_wire_read_bit(pin, delay) {
                value |= 1 << bit_index;
            }
        }

        *byte = value;
    }
}

fn ds18b20_issue_command(
    pin: &mut Flex<'static>,
    delay: &mut Delay,
    command: u8,
) -> Result<(), SensorError> {
    critical_section::with(|_| {
        one_wire_reset(pin, delay)?;
        one_wire_write_byte(pin, delay, ONEWIRE_SKIP_ROM);
        one_wire_write_byte(pin, delay, command);
        Ok(())
    })
}

fn ds18b20_read_scratchpad(
    pin: &mut Flex<'static>,
    delay: &mut Delay,
    scratchpad: &mut [u8; 9],
) -> Result<(), SensorError> {
    critical_section::with(|_| {
        one_wire_reset(pin, delay)?;
        one_wire_write_byte(pin, delay, ONEWIRE_SKIP_ROM);
        one_wire_write_byte(pin, delay, ONEWIRE_READ_SCRATCHPAD);
        one_wire_read_bytes(pin, delay, scratchpad);
        Ok(())
    })
}

/// Write the DS18B20 configuration register to set the ADC resolution.
///
/// `resolution_bits` must be 9, 10, 11, or 12. Values outside that range are
/// clamped. Call this once at start-up after the 1-Wire pin has been configured.
/// The register is stored in non-volatile EEPROM on the sensor so it survives
/// a power cycle, but writing it on every boot is cheap and keeps behaviour
/// deterministic.
pub fn ds18b20_configure_resolution(
    pin: &mut Flex<'static>,
    delay: &mut Delay,
    resolution_bits: u8,
) -> Result<(), SensorError> {
    // Config register byte: bits[6:5] = R1:R0 selects resolution.
    //   9-bit  → 0x1F, 10-bit → 0x3F, 11-bit → 0x5F, 12-bit → 0x7F
    let r = resolution_bits.clamp(9, 12) - 9; // 0..=3
    let config_byte: u8 = 0x1F | (r << 5);
    critical_section::with(|_| {
        one_wire_reset(pin, delay)?;
        one_wire_write_byte(pin, delay, ONEWIRE_SKIP_ROM);
        one_wire_write_byte(pin, delay, ONEWIRE_WRITE_SCRATCHPAD);
        one_wire_write_byte(pin, delay, 0x00); // TH register (unused)
        one_wire_write_byte(pin, delay, 0x00); // TL register (unused)
        one_wire_write_byte(pin, delay, config_byte);
        Ok(())
    })
}

pub fn ds18b20_start_conversion(
    pin: &mut Flex<'static>,
    delay: &mut Delay,
) -> Result<(), SensorError> {
    ds18b20_issue_command(pin, delay, ONEWIRE_CONVERT_TEMP)
}

pub fn ds18b20_read_temperature_c(
    pin: &mut Flex<'static>,
    delay: &mut Delay,
) -> Result<f32, SensorError> {
    for attempt in 0..DS18B20_READ_ATTEMPTS {
        let mut scratchpad = [0u8; 9];
        ds18b20_read_scratchpad(pin, delay, &mut scratchpad)?;

        if crc8_maxim(&scratchpad[..8]) != scratchpad[8] {
            if attempt + 1 < DS18B20_READ_ATTEMPTS {
                continue;
            }
            return Err(SensorError::CrcMismatch);
        }

        let raw = i16::from_le_bytes([scratchpad[0], scratchpad[1]]);
        return Ok(raw as f32 / 16.0);
    }

    Err(SensorError::CrcMismatch)
}

fn crc8_maxim(bytes: &[u8]) -> u8 {
    shared::crc8_maxim(bytes)
}
