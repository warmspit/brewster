// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

use core::cell::{Cell, RefCell};
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU16, AtomicU32, Ordering};
use critical_section::Mutex;
use embedded_storage::{ReadStorage, Storage};
use esp_bootloader_esp_idf::partitions::{PARTITION_TABLE_MAX_LEN, read_partition_table};
use esp_hal::peripherals::FLASH;
use esp_storage::FlashStorage;
use static_cell::ConstStaticCell;

use super::error::SensorError;
use super::{error, shared};

// Type alias for backward compatibility; PersistError is now StorageError from error.rs
pub use error::StorageError as PersistError;

#[repr(u8)]
#[derive(Clone, Copy)]
enum SensorStatus {
    None = 0,
    BusStuckLow = 1,
    NoDevice = 2,
    CrcMismatch = 3,
}

impl SensorStatus {
    fn from_u8(code: u8) -> Self {
        match code {
            1 => Self::BusStuckLow,
            2 => Self::NoDevice,
            3 => Self::CrcMismatch,
            _ => Self::None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::BusStuckLow => "bus_stuck_low",
            Self::NoDevice => "no_device",
            Self::CrcMismatch => "crc_mismatch",
        }
    }
}

impl From<SensorError> for SensorStatus {
    fn from(error: SensorError) -> Self {
        match error {
            SensorError::BusStuckLow => Self::BusStuckLow,
            SensorError::NoDevice => Self::NoDevice,
            SensorError::CrcMismatch => Self::CrcMismatch,
        }
    }
}

pub fn sensor_status_label(code: u8) -> &'static str {
    SensorStatus::from_u8(code).label()
}

const UNKNOWN_TEMPERATURE_CENTI: i32 = i32::MIN;
const TARGET_TEMP_MIN_CENTI: i32 = 0;
const TARGET_TEMP_MAX_CENTI: i32 = 15_000;
pub const TEMP_PROBE_NAME_MAX_LEN: usize = 32;
const TARGET_STORE_MAGIC: [u8; 4] = *b"BRWT";
const TARGET_STORE_VERSION: u8 = 1;
const TARGET_STORE_SIZE: usize = 9;
const TARGET_PARTITION_LABEL: &str = "cfg";
#[repr(u8)]
#[derive(Clone, Copy)]
pub enum NetState {
    Unknown = 0,
    LinkDown = 1,
    DhcpPending = 2,
    HasIp = 3,
}

impl NetState {
    pub fn from_u8(code: u8) -> Self {
        match code {
            1 => Self::LinkDown,
            2 => Self::DhcpPending,
            3 => Self::HasIp,
            _ => Self::Unknown,
        }
    }
}

const NTP_MAX_TRACKED_PEERS: usize = shared::NTP_MAX_CONFIG_SERVERS + 1;

static LAST_TEMP_CENTI: AtomicI32 = AtomicI32::new(UNKNOWN_TEMPERATURE_CENTI);
static LAST_PID_OUTPUT_DECI_PERCENT: AtomicU16 = AtomicU16::new(0);
static LAST_RELAY_ON: AtomicBool = AtomicBool::new(false);
static LAST_SENSOR_STATUS: AtomicU8 = AtomicU8::new(SensorStatus::NoDevice as u8);
static LAST_LED_RED: AtomicU8 = AtomicU8::new(0);
static LAST_LED_GREEN: AtomicU8 = AtomicU8::new(0);
static LAST_LED_BLUE: AtomicU8 = AtomicU8::new(0);
static HTTP_REQUEST_ACTIVITY: AtomicBool = AtomicBool::new(false);
static LAST_PID_WINDOW_STEP: AtomicU8 = AtomicU8::new(0);
static LAST_PID_ON_STEPS: AtomicU8 = AtomicU8::new(0);
// Device IP packed as big-endian u32; use .to_be_bytes() to recover [u8; 4].
static LAST_IP: AtomicU32 = AtomicU32::new(0);
static LAST_IP_VALID: AtomicBool = AtomicBool::new(false);
static LAST_NET_STATE: AtomicU8 = AtomicU8::new(NetState::Unknown as u8);
static NTP_SYNCED: AtomicBool = AtomicBool::new(false);
// Stores (recv_ticks_at_t4, t4_unix_micros): the tick counter and corrected wall-clock
// microseconds captured at packet receipt (T4), atomically under critical_section.
static NTP_SYNC_ANCHOR: Mutex<Cell<(u64, u64)>> = Mutex::new(Cell::new((0, 0)));
static NTP_SYNC_COUNT: AtomicU32 = AtomicU32::new(0);
// Source stored as NtpSource discriminant (see shared::NtpSource).
static NTP_SERVER_SOURCE: AtomicU8 = AtomicU8::new(0);
// NTP master server packed as big-endian u32; use .to_be_bytes() to recover [u8; 4].
static NTP_SERVER_IP: AtomicU32 = AtomicU32::new(0);
static NTP_PEERS: Mutex<RefCell<[Option<NtpPeerState>; NTP_MAX_TRACKED_PEERS]>> =
    Mutex::new(RefCell::new([None; NTP_MAX_TRACKED_PEERS]));
// Runtime-settable target temperature stored as centidegrees (2111 = 21.11°C = 70°F).
static TARGET_TEMP_CENTI: AtomicI32 = AtomicI32::new(2111);
static TARGET_STORE_OFFSET: AtomicU32 = AtomicU32::new(0);
static TARGET_STORE_PARTITION_LEN: AtomicU32 = AtomicU32::new(0);
static TEMP_PROBE_NAME: Mutex<RefCell<heapless::String<TEMP_PROBE_NAME_MAX_LEN>>> =
    Mutex::new(RefCell::new(heapless::String::new()));
static FLASH_STORAGE: Mutex<RefCell<Option<FlashStorage<'static>>>> =
    Mutex::new(RefCell::new(None));
static PARTITION_TABLE_BUFFER: ConstStaticCell<[u8; PARTITION_TABLE_MAX_LEN]> =
    ConstStaticCell::new([0; PARTITION_TABLE_MAX_LEN]);

fn valid_target_centi(target_centi: i32) -> bool {
    (TARGET_TEMP_MIN_CENTI..=TARGET_TEMP_MAX_CENTI).contains(&target_centi)
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

        storage.write(store_offset, &raw)?;
        Ok(())
    })
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

#[derive(Clone, Copy)]
struct NtpPeerState {
    address: [u8; 4],
    has_sample: bool,
    stratum: u8,
    latency_us: u32,
    last_latency_us: u32,
    jitter_us: u32,
    has_offset: bool,
    offset_us: i32,
    last_offset_us: i32,
    offset_jitter_us: u32,
    success_count: u32,
    fail_count: u32,
    last_sync_uptime_s: u32,
    source: shared::NtpSource,
}

impl NtpPeerState {
    const fn new(source: shared::NtpSource, address: [u8; 4]) -> Self {
        Self {
            address,
            has_sample: false,
            stratum: 0,
            latency_us: 0,
            last_latency_us: 0,
            jitter_us: 0,
            has_offset: false,
            offset_us: 0,
            last_offset_us: 0,
            offset_jitter_us: 0,
            success_count: 0,
            fail_count: 0,
            last_sync_uptime_s: 0,
            source,
        }
    }
}

#[derive(Clone, Copy)]
pub struct NtpPeerSnapshot {
    pub address: [u8; 4],
    pub has_sample: bool,
    pub stratum: u8,
    pub latency_us: u32,
    pub jitter_us: u32,
    pub offset_us: Option<i32>,
    pub offset_jitter_us: Option<u32>,
    pub success_count: u32,
    pub fail_count: u32,
    pub last_sync_uptime_s: u32,
    pub source: shared::NtpSource,
}

pub struct MetricsSnapshot {
    pub temp_centi: i32,
    pub pid_deci: u16,
    pub relay_on: bool,
    pub sensor_status_code: u8,
    pub led_red: u8,
    pub led_green: u8,
    pub led_blue: u8,
    pub pid_window_step: u8,
    pub pid_on_steps: u8,
    pub target_c: f32,
    pub target_f: f32,
    pub ip_valid: bool,
    pub net_state_code: u8,
    pub ip_octets: [u8; 4],
    pub ntp_synced: bool,
    pub ntp_sync_count: u32,
    pub ntp_source_code: u8,
    pub ntp_uptime_at_sync: u32,
    pub current_ntp_time: Option<u32>,
    pub master_ip: [u8; 4],
    pub probe_name: heapless::String<TEMP_PROBE_NAME_MAX_LEN>,
}

pub struct PrometheusSnapshot {
    pub temp_centi: i32,
    pub pid_deci: u16,
    pub pid_window_step: u8,
    pub pid_on_steps: u8,
    pub relay_on: bool,
    pub target_c: f32,
    pub target_f: f32,
    pub ntp_synced: bool,
    pub ntp_sync_count: u32,
    pub ntp_source_code: u8,
    pub ntp_uptime_at_sync: u32,
    pub master_ip: [u8; 4],
    pub probe_name: heapless::String<TEMP_PROBE_NAME_MAX_LEN>,
}

#[derive(Clone, Copy, Debug)]
pub enum ProbeNameError {
    Empty,
    TooLong,
    InvalidChar,
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

fn clear_ntp_peers_by_source(source: shared::NtpSource) {
    critical_section::with(|cs| {
        let mut peers = NTP_PEERS.borrow_ref_mut(cs);
        for slot in peers.iter_mut() {
            if slot.is_some_and(|peer| peer.source == source) {
                *slot = None;
            }
        }
    });
}

fn update_ntp_peer(
    source: shared::NtpSource,
    address: [u8; 4],
    update: impl FnOnce(&mut NtpPeerState),
) {
    critical_section::with(|cs| {
        let mut peers = NTP_PEERS.borrow_ref_mut(cs);
        let mut target_index = None;
        let mut empty_index = None;

        for (index, slot) in peers.iter_mut().enumerate() {
            match slot {
                Some(peer) if peer.source == source && peer.address == address => {
                    target_index = Some(index);
                    break;
                }
                None if empty_index.is_none() => empty_index = Some(index),
                _ => {}
            }
        }

        let Some(index) = target_index.or(empty_index) else {
            return;
        };

        let peer = peers[index].get_or_insert(NtpPeerState::new(source, address));
        if peer.source != source || peer.address != address {
            *peer = NtpPeerState::new(source, address);
        }
        update(peer);
    });
}

pub fn ntp_peers_snapshot() -> heapless::Vec<NtpPeerSnapshot, { shared::NTP_MAX_CONFIG_SERVERS + 1 }>
{
    critical_section::with(|cs| {
        let peers = NTP_PEERS.borrow_ref(cs);
        let mut snapshots = heapless::Vec::new();

        for peer in peers.iter().flatten() {
            let _ = snapshots.push(NtpPeerSnapshot {
                address: peer.address,
                has_sample: peer.has_sample,
                stratum: peer.stratum,
                latency_us: peer.latency_us,
                jitter_us: peer.jitter_us,
                offset_us: peer.has_offset.then_some(peer.offset_us),
                offset_jitter_us: peer.has_offset.then_some(peer.offset_jitter_us),
                success_count: peer.success_count,
                fail_count: peer.fail_count,
                last_sync_uptime_s: peer.last_sync_uptime_s,
                source: peer.source,
            });
        }

        snapshots
    })
}

/// Snapshot all metric state needed for JSON and text formatting.
pub fn metrics_snapshot() -> MetricsSnapshot {
    let temp_centi = LAST_TEMP_CENTI.load(Ordering::Relaxed);
    let pid_deci = LAST_PID_OUTPUT_DECI_PERCENT.load(Ordering::Relaxed);
    let relay_on = LAST_RELAY_ON.load(Ordering::Relaxed);
    let sensor_status_code = LAST_SENSOR_STATUS.load(Ordering::Relaxed);
    let led_red = LAST_LED_RED.load(Ordering::Relaxed);
    let led_green = LAST_LED_GREEN.load(Ordering::Relaxed);
    let led_blue = LAST_LED_BLUE.load(Ordering::Relaxed);
    let pid_window_step = LAST_PID_WINDOW_STEP.load(Ordering::Relaxed);
    let pid_on_steps = LAST_PID_ON_STEPS.load(Ordering::Relaxed);

    let target_c = TARGET_TEMP_CENTI.load(Ordering::Relaxed) as f32 / 100.0;
    let target_f = target_c * 9.0 / 5.0 + 32.0;
    let ip_valid = LAST_IP_VALID.load(Ordering::Relaxed);
    let net_state_code = LAST_NET_STATE.load(Ordering::Relaxed);
    let ip_octets = LAST_IP.load(Ordering::Relaxed).to_be_bytes();
    let ntp_synced = NTP_SYNCED.load(Ordering::Relaxed);
    let ntp_sync_count = NTP_SYNC_COUNT.load(Ordering::Relaxed);
    let ntp_source_code = NTP_SERVER_SOURCE.load(Ordering::Relaxed);
    let ntp_uptime_at_sync = {
        let (sync_ticks, _) = critical_section::with(|cs| NTP_SYNC_ANCHOR.borrow(cs).get());
        (sync_ticks / embassy_time::TICK_HZ) as u32
    };
    let current_ntp_time = current_unix_time();
    let master_ip = NTP_SERVER_IP.load(Ordering::Relaxed).to_be_bytes();
    let probe_name = temp_probe_name();

    MetricsSnapshot {
        temp_centi,
        pid_deci,
        relay_on,
        sensor_status_code,
        led_red,
        led_green,
        led_blue,
        pid_window_step,
        pid_on_steps,
        target_c,
        target_f,
        ip_valid,
        net_state_code,
        ip_octets,
        ntp_synced,
        ntp_sync_count,
        ntp_source_code,
        ntp_uptime_at_sync,
        current_ntp_time,
        master_ip,
        probe_name,
    }
}

/// Snapshot all metric state needed for Prometheus formatting.
pub fn prometheus_snapshot() -> PrometheusSnapshot {
    let temp_centi = LAST_TEMP_CENTI.load(Ordering::Relaxed);
    let pid_deci = LAST_PID_OUTPUT_DECI_PERCENT.load(Ordering::Relaxed);
    let pid_window_step = LAST_PID_WINDOW_STEP.load(Ordering::Relaxed);
    let pid_on_steps = LAST_PID_ON_STEPS.load(Ordering::Relaxed);
    let relay_on = LAST_RELAY_ON.load(Ordering::Relaxed);

    let target_c = TARGET_TEMP_CENTI.load(Ordering::Relaxed) as f32 / 100.0;
    let target_f = target_c * 9.0 / 5.0 + 32.0;
    let ntp_synced = NTP_SYNCED.load(Ordering::Relaxed);
    let ntp_sync_count = NTP_SYNC_COUNT.load(Ordering::Relaxed);
    let ntp_source_code = NTP_SERVER_SOURCE.load(Ordering::Relaxed);
    let ntp_uptime_at_sync = {
        let (sync_ticks, _) = critical_section::with(|cs| NTP_SYNC_ANCHOR.borrow(cs).get());
        (sync_ticks / embassy_time::TICK_HZ) as u32
    };
    let master_ip = NTP_SERVER_IP.load(Ordering::Relaxed).to_be_bytes();
    let probe_name = temp_probe_name();

    PrometheusSnapshot {
        temp_centi,
        pid_deci,
        pid_window_step,
        pid_on_steps,
        relay_on,
        target_c,
        target_f,
        ntp_synced,
        ntp_sync_count,
        ntp_source_code,
        ntp_uptime_at_sync,
        master_ip,
        probe_name,
    }
}

fn reset_dhcp_ntp_state() {
    clear_ntp_peers_by_source(shared::NtpSource::DhcpGateway);
}

fn mark_ip_invalid(state: NetState) {
    LAST_IP_VALID.store(false, Ordering::Relaxed);
    LAST_NET_STATE.store(state as u8, Ordering::Relaxed);
}

pub fn update_success(sample: RuntimeSample) {
    LAST_TEMP_CENTI.store((sample.temp_c * 100.0) as i32, Ordering::Relaxed);
    LAST_PID_OUTPUT_DECI_PERCENT.store((sample.pid_output * 10.0) as u16, Ordering::Relaxed);
    LAST_RELAY_ON.store(sample.heating_on, Ordering::Relaxed);
    LAST_SENSOR_STATUS.store(SensorStatus::None as u8, Ordering::Relaxed);
    LAST_LED_RED.store(sample.led_red, Ordering::Relaxed);
    LAST_LED_GREEN.store(sample.led_green, Ordering::Relaxed);
    LAST_LED_BLUE.store(sample.led_blue, Ordering::Relaxed);
    LAST_PID_WINDOW_STEP.store(sample.pid_window_step, Ordering::Relaxed);
    LAST_PID_ON_STEPS.store(sample.pid_on_steps, Ordering::Relaxed);
}

pub fn update_error(error: SensorError, led_red: u8, led_green: u8, led_blue: u8) {
    LAST_TEMP_CENTI.store(UNKNOWN_TEMPERATURE_CENTI, Ordering::Relaxed);
    LAST_PID_OUTPUT_DECI_PERCENT.store(0, Ordering::Relaxed);
    LAST_RELAY_ON.store(false, Ordering::Relaxed);
    LAST_SENSOR_STATUS.store(SensorStatus::from(error) as u8, Ordering::Relaxed);
    LAST_LED_RED.store(led_red, Ordering::Relaxed);
    LAST_LED_GREEN.store(led_green, Ordering::Relaxed);
    LAST_LED_BLUE.store(led_blue, Ordering::Relaxed);
    LAST_PID_WINDOW_STEP.store(0, Ordering::Relaxed);
    LAST_PID_ON_STEPS.store(0, Ordering::Relaxed);
}

pub fn update_led(led_red: u8, led_green: u8, led_blue: u8) {
    LAST_LED_RED.store(led_red, Ordering::Relaxed);
    LAST_LED_GREEN.store(led_green, Ordering::Relaxed);
    LAST_LED_BLUE.store(led_blue, Ordering::Relaxed);
}

pub fn update_ip_from_cidr(ip_or_cidr: &str) {
    let ip = ip_or_cidr.split('/').next().unwrap_or(ip_or_cidr);
    let mut octets = [0u8; 4];
    let mut count = 0usize;

    for part in ip.split('.') {
        if count >= 4 {
            mark_ip_invalid(NetState::DhcpPending);
            return;
        }

        let Ok(value) = part.parse::<u8>() else {
            mark_ip_invalid(NetState::DhcpPending);
            return;
        };

        octets[count] = value;
        count += 1;
    }

    if count == 4 {
        LAST_IP.store(u32::from_be_bytes(octets), Ordering::Relaxed);
        LAST_IP_VALID.store(true, Ordering::Relaxed);
        LAST_NET_STATE.store(NetState::HasIp as u8, Ordering::Relaxed);
    } else {
        mark_ip_invalid(NetState::DhcpPending);
    }
}

pub fn clear_ip() {
    mark_net_link_down();
}

pub fn mark_net_link_down() {
    mark_ip_invalid(NetState::LinkDown);
    reset_dhcp_ntp_state();
}

pub fn mark_net_dhcp_pending() {
    mark_ip_invalid(NetState::DhcpPending);
    reset_dhcp_ntp_state();
}

pub fn ip_octets() -> Option<[u8; 4]> {
    if !LAST_IP_VALID.load(Ordering::Relaxed) {
        return None;
    }

    Some(LAST_IP.load(Ordering::Relaxed).to_be_bytes())
}

pub fn update_ntp_time(
    unix_ts: u32,
    ntp_frac_us: u32,
    recv_ticks: u64,
    latency_us: u32,
    server: [u8; 4],
    source: shared::NtpSource,
    _stratum: u8,
) {
    // Best estimate of wall-clock time at T4 (client receive):
    //   T4 ≈ T3 + one_way_delay ≈ T3 + RTT/2
    let t4_unix_micros = unix_ts as u64 * 1_000_000 + ntp_frac_us as u64 + latency_us as u64 / 2;
    critical_section::with(|cs| {
        NTP_SYNC_ANCHOR.borrow(cs).set((recv_ticks, t4_unix_micros));
    });
    NTP_SYNCED.store(true, Ordering::Relaxed);
    NTP_SYNC_COUNT.fetch_add(1, Ordering::Relaxed);
    NTP_SERVER_IP.store(u32::from_be_bytes(server), Ordering::Relaxed);
    NTP_SERVER_SOURCE.store(source as u8, Ordering::Relaxed);
}

pub fn update_dhcp_ntp_server(server: [u8; 4]) {
    critical_section::with(|cs| {
        let mut peers = NTP_PEERS.borrow_ref_mut(cs);
        for slot in peers.iter_mut() {
            if slot.is_some_and(|peer| {
                peer.source == shared::NtpSource::DhcpGateway && peer.address != server
            }) {
                *slot = None;
            }
        }
    });
    update_ntp_peer(shared::NtpSource::DhcpGateway, server, |_| {});
}

pub fn update_ntp_peer_stats(
    source: shared::NtpSource,
    server: [u8; 4],
    stratum: u8,
    latency_us: u32,
    offset_us: Option<i32>,
) {
    let uptime_s = (embassy_time::Instant::now().as_ticks() / embassy_time::TICK_HZ) as u32;
    update_ntp_peer(source, server, |peer| {
        peer.stratum = stratum;
        peer.latency_us = latency_us;
        if peer.has_sample {
            let delta = latency_us.abs_diff(peer.last_latency_us);
            peer.jitter_us = (peer.jitter_us.saturating_mul(7).saturating_add(delta)) / 8;
        } else {
            peer.jitter_us = 0;
            peer.has_sample = true;
        }
        peer.last_latency_us = latency_us;
        if let Some(offset_us) = offset_us {
            if peer.has_offset {
                let delta = offset_us.abs_diff(peer.last_offset_us);
                peer.offset_jitter_us = (peer
                    .offset_jitter_us
                    .saturating_mul(7)
                    .saturating_add(delta))
                    / 8;
            } else {
                peer.offset_jitter_us = 0;
                peer.has_offset = true;
            }
            peer.offset_us = offset_us;
            peer.last_offset_us = offset_us;
        }
        peer.success_count = peer.success_count.saturating_add(1);
        peer.last_sync_uptime_s = uptime_s;
    });
}

fn micros_to_ms_rounded(us: u32) -> u32 {
    us.saturating_add(500) / 1_000
}

fn micros_to_ms_ceil_nonzero(us: u32) -> u32 {
    if us == 0 {
        0
    } else {
        us.saturating_add(999) / 1_000
    }
}

pub fn ntp_selection_sample(
    source: shared::NtpSource,
    server: [u8; 4],
) -> Option<shared::NtpSelectionSample> {
    critical_section::with(|cs| {
        let peers = NTP_PEERS.borrow_ref(cs);
        peers
            .iter()
            .flatten()
            .find(|peer| peer.source == source && peer.address == server && peer.has_sample)
            .map(|peer| shared::NtpSelectionSample {
                stratum: peer.stratum,
                latency_ms: micros_to_ms_rounded(peer.latency_us),
                jitter_ms: if peer.has_offset {
                    micros_to_ms_ceil_nonzero(peer.offset_jitter_us)
                } else {
                    micros_to_ms_ceil_nonzero(peer.jitter_us)
                },
            })
    })
}

pub fn mark_ntp_peer_query_failed(source: shared::NtpSource, server: [u8; 4]) {
    update_ntp_peer(source, server, |peer| {
        peer.fail_count = peer.fail_count.saturating_add(1);
    });
}

pub fn http_request_received() {
    HTTP_REQUEST_ACTIVITY.store(true, Ordering::Relaxed);
}

pub fn http_request_activity() -> bool {
    HTTP_REQUEST_ACTIVITY.swap(false, Ordering::Relaxed)
}

pub fn current_unix_time_micros() -> Option<u64> {
    if !NTP_SYNCED.load(Ordering::Relaxed) {
        return None;
    }
    let now_ticks = embassy_time::Instant::now().as_ticks();
    let (recv_ticks, t4_unix_micros) =
        critical_section::with(|cs| NTP_SYNC_ANCHOR.borrow(cs).get());
    let elapsed_ticks = now_ticks.saturating_sub(recv_ticks);
    let elapsed_us = elapsed_ticks.saturating_mul(1_000_000) / embassy_time::TICK_HZ;
    Some(t4_unix_micros.saturating_add(elapsed_us))
}

pub fn current_unix_time_millis() -> Option<u64> {
    current_unix_time_micros().map(|us| us / 1_000)
}

pub fn current_unix_time() -> Option<u32> {
    current_unix_time_millis().map(|ms| (ms / 1_000) as u32)
}
