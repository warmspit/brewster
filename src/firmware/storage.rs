// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

//! Flash-backed persistence: target temperature, history ring buffer, and probe name.

use alloc::vec::Vec;
use core::cell::RefCell;
use core::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use critical_section::Mutex;
use embedded_storage::nor_flash::NorFlash;
use embedded_storage::{ReadStorage, Storage};
use esp_bootloader_esp_idf::partitions::{PARTITION_TABLE_MAX_LEN, read_partition_table};
use esp_hal::peripherals::FLASH;
use esp_storage::FlashStorage;
use static_cell::ConstStaticCell;

use super::error::StorageError as PersistError;

pub const TEMP_PROBE_NAME_MAX_LEN: usize = 32;

const TARGET_TEMP_MIN_CENTI: i32 = -2_000;
const TARGET_TEMP_MAX_CENTI: i32 = 2_500;
const TARGET_STORE_MAGIC: [u8; 4] = *b"BRWT";
const TARGET_STORE_VERSION: u8 = 1;
const TARGET_STORE_SIZE: usize = 9;
const TARGET_PARTITION_LABEL: &str = "cfg";
const HISTORY_RECORD_SIZE: u32 = 16;
/// Number of extra (non-control) sensor temperatures stored per record.
const HISTORY_EXTRA_SENSOR_COUNT: usize = 3;
/// Sentinel i16 value meaning "no reading available" for an extra sensor slot.
const HISTORY_EXTRA_SENSOR_NONE: i16 = i16::MAX;
const HISTORY_DATA_OFFSET: u32 = 0x1000;
const HISTORY_SECTOR_SIZE: u32 = 0x1000;
const HISTORY_SAMPLE_INTERVAL_SECS: u32 = 60;

#[derive(Clone, Copy)]
pub struct HistorySample {
    pub seq: u32,
    pub temp_c: f32,
    pub target_c: f32,
    pub output_percent: f32,
    pub window_step: u8,
    pub on_steps: u8,
    pub relay_on: bool,
    /// Extra sensor temperatures (index 0 = sensor 1, etc.).
    /// `f32::NAN` means no reading was available when the record was written.
    pub extra_temps: [f32; HISTORY_EXTRA_SENSOR_COUNT],
}

pub struct RuntimeSample {
    pub temp_c: f32,
    pub pid_output: f32,
    pub heating_on: bool,
    pub led_red: u8,
    pub led_green: u8,
    pub led_blue: u8,
    pub pid_window_step: u8,
    pub pid_on_steps: u8,
}

#[derive(Clone, Copy, Debug)]
pub enum ProbeNameError {
    Empty,
    TooLong,
    InvalidChar,
}

// Flash-backed state: target temperature and history ring buffer.
static TARGET_TEMP_CENTI: AtomicI32 = AtomicI32::new(2111);
static TARGET_STORE_OFFSET: AtomicU32 = AtomicU32::new(0);
static TARGET_STORE_PARTITION_LEN: AtomicU32 = AtomicU32::new(0);
static HISTORY_BASE_OFFSET: AtomicU32 = AtomicU32::new(0);
static HISTORY_CAPACITY: AtomicU32 = AtomicU32::new(0);
static HISTORY_WRITE_INDEX: AtomicU32 = AtomicU32::new(0);
static HISTORY_NEXT_SEQ: AtomicU32 = AtomicU32::new(0);
static HISTORY_COUNT: AtomicU32 = AtomicU32::new(0);
static HISTORY_LAST_PERSIST_UPTIME_S: AtomicU32 = AtomicU32::new(0);
// RAM-only probe name (not flash-backed).
static TEMP_PROBE_NAME: Mutex<RefCell<heapless::String<TEMP_PROBE_NAME_MAX_LEN>>> =
    Mutex::new(RefCell::new(heapless::String::new()));
static FLASH_STORAGE: Mutex<RefCell<Option<FlashStorage<'static>>>> =
    Mutex::new(RefCell::new(None));
static PARTITION_TABLE_BUFFER: ConstStaticCell<[u8; PARTITION_TABLE_MAX_LEN]> =
    ConstStaticCell::new([0; PARTITION_TABLE_MAX_LEN]);

fn valid_target_centi(target_centi: i32) -> bool {
    (TARGET_TEMP_MIN_CENTI..=TARGET_TEMP_MAX_CENTI).contains(&target_centi)
}

fn history_record_valid(raw: &[u8; HISTORY_RECORD_SIZE as usize]) -> bool {
    u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) != u32::MAX
}

fn history_record_offset(index: u32) -> u32 {
    HISTORY_BASE_OFFSET.load(Ordering::Relaxed) + index.saturating_mul(HISTORY_RECORD_SIZE)
}

fn history_init(storage: &mut FlashStorage<'static>, partition_offset: u32, partition_len: u32) {
    if partition_len <= HISTORY_DATA_OFFSET + HISTORY_RECORD_SIZE {
        HISTORY_BASE_OFFSET.store(0, Ordering::Relaxed);
        HISTORY_CAPACITY.store(0, Ordering::Relaxed);
        HISTORY_WRITE_INDEX.store(0, Ordering::Relaxed);
        HISTORY_NEXT_SEQ.store(0, Ordering::Relaxed);
        HISTORY_COUNT.store(0, Ordering::Relaxed);
        return;
    }

    let base = partition_offset + HISTORY_DATA_OFFSET;
    let capacity = (partition_len - HISTORY_DATA_OFFSET) / HISTORY_RECORD_SIZE;
    HISTORY_BASE_OFFSET.store(base, Ordering::Relaxed);
    HISTORY_CAPACITY.store(capacity, Ordering::Relaxed);

    let mut raw = [0u8; HISTORY_RECORD_SIZE as usize];
    let mut max_seq = 0u32;
    let mut max_index = 0u32;
    let mut has_records = false;
    let mut valid_count = 0u32;

    for index in 0..capacity {
        let offset = base + index.saturating_mul(HISTORY_RECORD_SIZE);
        if storage.read(offset, &mut raw).is_err() {
            continue;
        }
        if !history_record_valid(&raw) {
            continue;
        }
        valid_count = valid_count.saturating_add(1);
        let seq = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        if !has_records || seq > max_seq {
            has_records = true;
            max_seq = seq;
            max_index = index;
        }
    }

    if has_records {
        HISTORY_WRITE_INDEX.store((max_index + 1) % capacity, Ordering::Relaxed);
        HISTORY_NEXT_SEQ.store(max_seq.wrapping_add(1), Ordering::Relaxed);
        HISTORY_COUNT.store(valid_count, Ordering::Relaxed);
    } else {
        HISTORY_WRITE_INDEX.store(0, Ordering::Relaxed);
        HISTORY_NEXT_SEQ.store(0, Ordering::Relaxed);
        HISTORY_COUNT.store(0, Ordering::Relaxed);
    }
}

/// Persist one history sample.  The caller is responsible for checking
/// `collection_enabled()` before calling this function.
///
/// `extra_temps_centi` holds centidegree readings for sensor indices 1..
/// (i.e. the first element is sensor index 1).  Missing/unavailable readings
/// should be encoded as `i32::MAX`.
pub(crate) fn persist_history_sample(sample: &RuntimeSample, extra_temps_centi: &[i32]) {
    let now_uptime_s = (embassy_time::Instant::now().as_ticks() / embassy_time::TICK_HZ) as u32;
    let last = HISTORY_LAST_PERSIST_UPTIME_S.load(Ordering::Relaxed);
    if last != 0 && now_uptime_s.saturating_sub(last) < HISTORY_SAMPLE_INTERVAL_SECS {
        return;
    }

    let capacity = HISTORY_CAPACITY.load(Ordering::Relaxed);
    if capacity == 0 {
        return;
    }

    HISTORY_LAST_PERSIST_UPTIME_S.store(now_uptime_s, Ordering::Relaxed);

    let target_centi = TARGET_TEMP_CENTI.load(Ordering::Relaxed) as i16;
    let temp_centi = (sample.temp_c * 100.0) as i16;
    // output: 1 % resolution (u8 0–100); window/on_steps packed into nibbles of flags byte.
    let output_pct = sample.pid_output.clamp(0.0, 100.0) as u8;
    let flags = (sample.pid_window_step.min(15) << 4) | sample.pid_on_steps.min(15);

    // Encode extra sensor temps; sentinel i16::MAX when reading is unavailable.
    let mut extra_centi = [HISTORY_EXTRA_SENSOR_NONE; HISTORY_EXTRA_SENSOR_COUNT];
    for (slot, raw_centi) in extra_centi.iter_mut().zip(extra_temps_centi.iter()) {
        if *raw_centi != i32::MAX {
            *slot = (*raw_centi).clamp(i16::MIN as i32, (i16::MAX - 1) as i32) as i16;
        }
    }

    critical_section::with(|cs| {
        let mut guard = FLASH_STORAGE.borrow_ref_mut(cs);
        let Some(storage) = guard.as_mut() else {
            return;
        };

        let write_index = HISTORY_WRITE_INDEX.load(Ordering::Relaxed);
        let mut count = HISTORY_COUNT.load(Ordering::Relaxed);
        let offset = history_record_offset(write_index);
        let records_per_sector = HISTORY_SECTOR_SIZE / HISTORY_RECORD_SIZE;

        if offset.is_multiple_of(HISTORY_SECTOR_SIZE) {
            if storage.erase(offset, offset + HISTORY_SECTOR_SIZE).is_err() {
                return;
            }
            let removed = core::cmp::min(count, records_per_sector);
            count = count.saturating_sub(removed);
        }

        let seq = HISTORY_NEXT_SEQ.load(Ordering::Relaxed);
        // Record layout (16 bytes):
        //  [0..4]   seq: u32 LE
        //  [4..6]   temp_centi[0]: i16 LE  (control probe; i16::MAX = no reading)
        //  [6..8]   temp_centi[1]: i16 LE  (sensor 1; i16::MAX = no reading)
        //  [8..10]  temp_centi[2]: i16 LE  (sensor 2; i16::MAX = no reading)
        //  [10..12] temp_centi[3]: i16 LE  (sensor 3; i16::MAX = no reading)
        //  [12..14] target_centi: i16 LE
        //  [14]     output_pct: u8          (0–100 %)
        //  [15]     flags: u8               hi-nibble = window_step (0–15),
        //                                   lo-nibble = on_steps (0–15)
        //           relay_on is derived on read: on_steps > 0 && window_step < on_steps
        let mut raw = [0xFFu8; HISTORY_RECORD_SIZE as usize];
        raw[0..4].copy_from_slice(&seq.to_le_bytes());
        raw[4..6].copy_from_slice(&temp_centi.to_le_bytes());
        raw[6..8].copy_from_slice(&extra_centi[0].to_le_bytes());
        raw[8..10].copy_from_slice(&extra_centi[1].to_le_bytes());
        raw[10..12].copy_from_slice(&extra_centi[2].to_le_bytes());
        raw[12..14].copy_from_slice(&target_centi.to_le_bytes());
        raw[14] = output_pct;
        raw[15] = flags;

        if Storage::write(storage, offset, &raw).is_err() {
            return;
        }

        HISTORY_WRITE_INDEX.store((write_index + 1) % capacity, Ordering::Relaxed);
        HISTORY_NEXT_SEQ.store(seq.wrapping_add(1), Ordering::Relaxed);
        HISTORY_COUNT.store(
            core::cmp::min(count.saturating_add(1), capacity),
            Ordering::Relaxed,
        );
    });
}

pub fn clear_history_persistent() -> Result<(), PersistError> {
    let capacity = HISTORY_CAPACITY.load(Ordering::Relaxed);
    if capacity == 0 {
        return Err(PersistError::MissingPartition);
    }
    let start = HISTORY_BASE_OFFSET.load(Ordering::Relaxed);
    let end = start + capacity.saturating_mul(HISTORY_RECORD_SIZE);

    critical_section::with(|cs| {
        let mut guard = FLASH_STORAGE.borrow_ref_mut(cs);
        let Some(storage) = guard.as_mut() else {
            return Err(PersistError::NotInitialized);
        };

        let mut sector = start;
        while sector < end {
            storage.erase(sector, sector + HISTORY_SECTOR_SIZE)?;
            sector += HISTORY_SECTOR_SIZE;
        }
        Ok(())
    })?;

    HISTORY_WRITE_INDEX.store(0, Ordering::Relaxed);
    HISTORY_NEXT_SEQ.store(0, Ordering::Relaxed);
    HISTORY_COUNT.store(0, Ordering::Relaxed);
    HISTORY_LAST_PERSIST_UPTIME_S.store(0, Ordering::Relaxed);
    Ok(())
}

pub fn history_sample_interval_secs() -> u32 {
    HISTORY_SAMPLE_INTERVAL_SECS
}

pub fn history_total_samples() -> u32 {
    HISTORY_COUNT.load(Ordering::Relaxed)
}

pub fn history_snapshot(max_points: usize) -> Vec<HistorySample> {
    let capacity = HISTORY_CAPACITY.load(Ordering::Relaxed);
    if capacity == 0 || max_points == 0 {
        return Vec::new();
    }

    let start_index = HISTORY_WRITE_INDEX.load(Ordering::Relaxed);

    critical_section::with(|cs| {
        let mut guard = FLASH_STORAGE.borrow_ref_mut(cs);
        let Some(storage) = guard.as_mut() else {
            return Vec::new();
        };

        let mut raw = [0u8; HISTORY_RECORD_SIZE as usize];
        let mut valid_count = 0usize;
        for step in 0..capacity {
            let index = (start_index + step) % capacity;
            if storage
                .read(history_record_offset(index), &mut raw)
                .is_err()
            {
                continue;
            }
            if history_record_valid(&raw) {
                valid_count += 1;
            }
        }

        if valid_count == 0 {
            return Vec::new();
        }

        let keep = core::cmp::min(max_points, valid_count);
        let skip = valid_count.saturating_sub(keep);
        let mut out = Vec::with_capacity(keep);
        let mut seen = 0usize;

        for step in 0..capacity {
            let index = (start_index + step) % capacity;
            if storage
                .read(history_record_offset(index), &mut raw)
                .is_err()
            {
                continue;
            }
            if !history_record_valid(&raw) {
                continue;
            }

            if seen >= skip {
                let seq = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
                let temp_centi = i16::from_le_bytes([raw[4], raw[5]]);
                let ec1 = i16::from_le_bytes([raw[6], raw[7]]);
                let ec2 = i16::from_le_bytes([raw[8], raw[9]]);
                let ec3 = i16::from_le_bytes([raw[10], raw[11]]);
                let target_centi = i16::from_le_bytes([raw[12], raw[13]]);
                let window_step = raw[15] >> 4;
                let on_steps = raw[15] & 0x0F;
                let decode = |ec: i16| -> f32 {
                    if ec == HISTORY_EXTRA_SENSOR_NONE {
                        f32::NAN
                    } else {
                        ec as f32 / 100.0
                    }
                };
                out.push(HistorySample {
                    seq,
                    temp_c: temp_centi as f32 / 100.0,
                    target_c: target_centi as f32 / 100.0,
                    output_percent: raw[14] as f32,
                    window_step,
                    on_steps,
                    relay_on: on_steps > 0 && window_step < on_steps,
                    extra_temps: [decode(ec1), decode(ec2), decode(ec3)],
                });
            }
            seen += 1;
        }

        out
    })
}

#[allow(
    clippy::large_stack_frames,
    reason = "partition table parsing requires a fixed-size temporary buffer"
)]
pub fn init_persistent_target(flash: FLASH<'static>) -> Option<f32> {
    let mut storage = FlashStorage::new(flash);
    let mut loaded = None;
    let partition_table_buf = PARTITION_TABLE_BUFFER.take();

    if let Ok(partition_table) = read_partition_table(&mut storage, partition_table_buf) {
        for entry in partition_table.iter() {
            if entry.label_as_str() == TARGET_PARTITION_LABEL {
                TARGET_STORE_OFFSET.store(entry.offset(), Ordering::Relaxed);
                TARGET_STORE_PARTITION_LEN.store(entry.len(), Ordering::Relaxed);
                break;
            }
        }
    }

    let store_offset = TARGET_STORE_OFFSET.load(Ordering::Relaxed);
    let store_len = TARGET_STORE_PARTITION_LEN.load(Ordering::Relaxed);

    if store_len >= TARGET_STORE_SIZE as u32 {
        let mut raw = [0u8; TARGET_STORE_SIZE];
        if storage.read(store_offset, &mut raw).is_ok()
            && raw[0..4] == TARGET_STORE_MAGIC
            && raw[4] == TARGET_STORE_VERSION
        {
            let target_centi = i32::from_le_bytes([raw[5], raw[6], raw[7], raw[8]]);
            if valid_target_centi(target_centi) {
                TARGET_TEMP_CENTI.store(target_centi, Ordering::Relaxed);
                loaded = Some(target_centi as f32 / 100.0);
            }
        }
    }

    history_init(&mut storage, store_offset, store_len);

    critical_section::with(|cs| {
        FLASH_STORAGE.borrow_ref_mut(cs).replace(storage);
    });

    loaded
}

pub fn get_target_temp_c() -> f32 {
    TARGET_TEMP_CENTI.load(Ordering::Relaxed) as f32 / 100.0
}

pub fn set_target_temp_c_persistent(target_c: f32) -> Result<(), PersistError> {
    let scaled = target_c * 100.0;
    let target_centi = if scaled >= 0.0 {
        (scaled + 0.5) as i32
    } else {
        (scaled - 0.5) as i32
    };
    if !valid_target_centi(target_centi) {
        return Err(PersistError::OutOfRange);
    }

    TARGET_TEMP_CENTI.store(target_centi, Ordering::Relaxed);
    persist_target_temp_c(target_centi)
}

fn persist_target_temp_c(target_centi: i32) -> Result<(), PersistError> {
    let store_offset = TARGET_STORE_OFFSET.load(Ordering::Relaxed);
    let store_len = TARGET_STORE_PARTITION_LEN.load(Ordering::Relaxed);

    if store_len == 0 {
        return Err(PersistError::MissingPartition);
    }
    if store_len < TARGET_STORE_SIZE as u32 {
        return Err(PersistError::PartitionTooSmall);
    }

    critical_section::with(|cs| {
        let mut guard = FLASH_STORAGE.borrow_ref_mut(cs);
        let Some(storage) = guard.as_mut() else {
            return Err(PersistError::NotInitialized);
        };

        let mut raw = [0u8; TARGET_STORE_SIZE];
        raw[0..4].copy_from_slice(&TARGET_STORE_MAGIC);
        raw[4] = TARGET_STORE_VERSION;
        raw[5..9].copy_from_slice(&target_centi.to_le_bytes());

        Storage::write(storage, store_offset, &raw)?;
        Ok(())
    })
}

fn is_valid_probe_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, ' ' | '-' | '_' | '.')
}

pub fn set_temp_probe_name(name: &str) -> Result<(), ProbeNameError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(ProbeNameError::Empty);
    }
    if trimmed.len() > TEMP_PROBE_NAME_MAX_LEN {
        return Err(ProbeNameError::TooLong);
    }
    if !trimmed.chars().all(is_valid_probe_name_char) {
        return Err(ProbeNameError::InvalidChar);
    }

    let mut normalized = heapless::String::<TEMP_PROBE_NAME_MAX_LEN>::new();
    normalized
        .push_str(trimmed)
        .map_err(|_| ProbeNameError::TooLong)?;

    critical_section::with(|cs| {
        *TEMP_PROBE_NAME.borrow_ref_mut(cs) = normalized;
    });
    Ok(())
}

pub fn temp_probe_name() -> heapless::String<TEMP_PROBE_NAME_MAX_LEN> {
    critical_section::with(|cs| {
        let current = TEMP_PROBE_NAME.borrow_ref(cs);
        if current.is_empty() {
            let mut fallback = heapless::String::new();
            let _ = fallback.push_str("probe-1");
            fallback
        } else {
            current.clone()
        }
    })
}
