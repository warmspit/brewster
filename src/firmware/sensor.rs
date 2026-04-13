// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

use embedded_hal::delay::DelayNs;
use esp_hal::delay::Delay;
use esp_hal::gpio::{DriveMode, Flex, InputConfig, OutputConfig, Pull};

use super::error::SensorError;
use super::shared;

const ONEWIRE_SKIP_ROM: u8 = 0xCC;
const ONEWIRE_MATCH_ROM: u8 = 0x55;
const ONEWIRE_SEARCH_ROM: u8 = 0xF0;
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

fn one_wire_write_rom(pin: &mut Flex<'static>, delay: &mut Delay, rom: [u8; 8]) {
    for byte in rom {
        one_wire_write_byte(pin, delay, byte);
    }
}

fn ds18b20_issue_command(
    pin: &mut Flex<'static>,
    delay: &mut Delay,
    rom: Option<[u8; 8]>,
    command: u8,
) -> Result<(), SensorError> {
    critical_section::with(|_| {
        one_wire_reset(pin, delay)?;
        if let Some(rom) = rom {
            one_wire_write_byte(pin, delay, ONEWIRE_MATCH_ROM);
            one_wire_write_rom(pin, delay, rom);
        } else {
            one_wire_write_byte(pin, delay, ONEWIRE_SKIP_ROM);
        }
        one_wire_write_byte(pin, delay, command);
        Ok(())
    })
}

fn ds18b20_read_scratchpad(
    pin: &mut Flex<'static>,
    delay: &mut Delay,
    rom: Option<[u8; 8]>,
    scratchpad: &mut [u8; 9],
) -> Result<(), SensorError> {
    critical_section::with(|_| {
        one_wire_reset(pin, delay)?;
        if let Some(rom) = rom {
            one_wire_write_byte(pin, delay, ONEWIRE_MATCH_ROM);
            one_wire_write_rom(pin, delay, rom);
        } else {
            one_wire_write_byte(pin, delay, ONEWIRE_SKIP_ROM);
        }
        one_wire_write_byte(pin, delay, ONEWIRE_READ_SCRATCHPAD);
        one_wire_read_bytes(pin, delay, scratchpad);
        Ok(())
    })
}

pub fn parse_ds18b20_serial(serial: &str) -> Option<[u8; 8]> {
    let mut compact = [0u8; 16];
    let mut used = 0usize;

    for b in serial.bytes() {
        if matches!(b, b':' | b'-' | b' ' | b'\t') {
            continue;
        }
        if used >= compact.len() {
            return None;
        }
        compact[used] = b;
        used += 1;
    }

    if used != 16 {
        return None;
    }

    let mut rom = [0u8; 8];
    for i in 0..8 {
        let hi = (compact[i * 2] as char).to_digit(16)? as u8;
        let lo = (compact[i * 2 + 1] as char).to_digit(16)? as u8;
        rom[i] = (hi << 4) | lo;
    }

    Some(rom)
}

pub fn format_ds18b20_serial(rom: [u8; 8]) -> heapless::String<16> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = heapless::String::<16>::new();
    for byte in rom {
        let _ = out.push(HEX[(byte >> 4) as usize] as char);
        let _ = out.push(HEX[(byte & 0x0F) as usize] as char);
    }
    out
}

pub fn ds18b20_scan_roms<const MAX: usize>(
    pin: &mut Flex<'static>,
    delay: &mut Delay,
) -> heapless::Vec<[u8; 8], MAX> {
    let mut found = heapless::Vec::<[u8; 8], MAX>::new();
    let mut last_discrepancy = 0u8;
    let mut last_device = false;
    let mut rom = [0u8; 8];

    while !last_device {
        let mut bit_number = 1u8;
        let mut next_discrepancy = 0u8;
        let mut next_rom = [0u8; 8];

        let search_ok = critical_section::with(|_| -> Result<bool, SensorError> {
            one_wire_reset(pin, delay)?;
            one_wire_write_byte(pin, delay, ONEWIRE_SEARCH_ROM);

            while bit_number <= 64 {
                let id_bit = one_wire_read_bit(pin, delay);
                let cmp_id_bit = one_wire_read_bit(pin, delay);

                let take_one = match (id_bit, cmp_id_bit) {
                    (true, true) => return Ok(false),
                    (false, false) => {
                        if bit_number < last_discrepancy {
                            (rom[(bit_number as usize - 1) / 8] >> ((bit_number as usize - 1) % 8))
                                & 1
                                == 1
                        } else if bit_number == last_discrepancy {
                            true
                        } else {
                            false
                        }
                    }
                    (false, true) => false,
                    (true, false) => true,
                };

                if !id_bit && !cmp_id_bit && !take_one {
                    next_discrepancy = bit_number;
                }

                let byte_index = (bit_number as usize - 1) / 8;
                let bit_index = (bit_number as usize - 1) % 8;
                if take_one {
                    next_rom[byte_index] |= 1 << bit_index;
                }

                one_wire_write_bit(pin, delay, take_one);
                bit_number += 1;
            }

            Ok(true)
        });

        let Ok(true) = search_ok else {
            break;
        };

        if crc8_maxim(&next_rom[..7]) != next_rom[7] {
            break;
        }

        if found.push(next_rom).is_err() {
            break;
        }

        rom = next_rom;
        last_discrepancy = next_discrepancy;
        if last_discrepancy == 0 {
            last_device = true;
        }
    }

    found
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
    ds18b20_issue_command(pin, delay, None, ONEWIRE_CONVERT_TEMP)
}

pub fn ds18b20_read_temperature_c_for(
    pin: &mut Flex<'static>,
    delay: &mut Delay,
    rom: Option<[u8; 8]>,
) -> Result<f32, SensorError> {
    for attempt in 0..DS18B20_READ_ATTEMPTS {
        let mut scratchpad = [0u8; 9];
        ds18b20_read_scratchpad(pin, delay, rom, &mut scratchpad)?;

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

#[allow(dead_code)]
pub fn ds18b20_read_temperature_c(
    pin: &mut Flex<'static>,
    delay: &mut Delay,
) -> Result<f32, SensorError> {
    ds18b20_read_temperature_c_for(pin, delay, None)
}

fn crc8_maxim(bytes: &[u8]) -> u8 {
    shared::crc8_maxim(bytes)
}
