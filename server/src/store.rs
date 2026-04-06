// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

//! In-memory ring buffer with configurable retention.
//!
//! Records are stored in arrival order.  Old records are pruned
//! whenever a new one is inserted.

use std::collections::VecDeque;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::packet::{PACKET_VERSION, Packet};

/// Store one record per this many seconds per uptime elapsed.
/// At 1s resolution the device sends one packet per second, so every packet is stored.
const STORE_INTERVAL_S: u32 = 1;

#[derive(Clone)]
pub struct Record {
    pub received_at: SystemTime,
    pub packet: Packet,
}

/// Thread-safe history store.
#[derive(Clone)]
pub struct Store(Arc<RwLock<Inner>>);

struct Inner {
    records: VecDeque<Record>,
    retention: Duration,
    /// Most recently received packet — always updated regardless of collecting state.
    latest: Option<Packet>,
    /// Whether the server is currently recording history into the ring buffer.
    collecting: bool,
    /// Total packets received.
    packets_received: u64,
    /// Inferred dropped packets (gaps in seq).
    packets_dropped: u64,
    /// Sequence number of the most recently received packet.
    last_seq: Option<u32>,
}

impl Store {
    pub fn new(retention: Duration) -> Self {
        Store(Arc::new(RwLock::new(Inner {
            records: VecDeque::new(),
            retention,
            latest: None,
            collecting: false,
            packets_received: 0,
            packets_dropped: 0,
            last_seq: None,
        })))
    }

    /// Insert a new packet. Always updates the latest packet for live /status reads.
    /// Only appends to the history ring buffer when collecting is enabled.
    /// Returns the persisted record if one was written to the ring buffer, else `None`.
    pub fn insert(&self, pkt: Packet) -> Option<PersistedRecord> {
        let mut g = self.0.write().unwrap();
        let now = SystemTime::now();

        // Detect dropped packets using sequence number gaps.
        if let Some(last_seq) = g.last_seq {
            let expected = last_seq.wrapping_add(1);
            let gap = pkt.seq.wrapping_sub(expected);
            // gap==0: consecutive (good). gap>=0x8000_0000: backwards jump (reset/reorder) — ignore.
            if gap > 0 && gap < 0x8000_0000 {
                g.packets_dropped += gap as u64;
            }
        }
        g.packets_received += 1;
        g.last_seq = Some(pkt.seq);

        // Mirror the device's collection state so both dashboards stay in sync.
        g.collecting = pkt.collecting;

        if g.collecting {
            g.latest = Some(pkt.clone());

            // Subsample: store one record per STORE_INTERVAL_S of device uptime so the
            // ring buffer stays a manageable size over the full retention window.
            let should_store = match g.records.back() {
                None => true,
                Some(last) => {
                    let last_up = last.packet.uptime_s;
                    pkt.uptime_s < last_up // device reboot
                        || pkt.uptime_s.saturating_sub(last_up) >= STORE_INTERVAL_S
                }
            };

            if should_store {
                let t = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
                let record = PersistedRecord {
                    t,
                    seq: pkt.seq,
                    uptime_s: pkt.uptime_s,
                    temp0: nan_to_null(pkt.temps[0]),
                    temp1: nan_to_null(pkt.temps[1]),
                    temp2: nan_to_null(pkt.temps[2]),
                    target_c: pkt.target_c,
                    output_pct: pkt.output_pct,
                    relay_on: pkt.relay_on,
                    device_collecting: pkt.collecting,
                    ntp_synced: pkt.ntp_synced,
                    window_step: pkt.window_step,
                    on_steps: pkt.on_steps,
                    sensor_status: pkt.sensor_status,
                    device_ip: pkt.device_ip,
                    sensor_count: pkt.sensor_count,
                    pid_p_pct: pkt.pid_p_pct,
                    pid_i_pct: pkt.pid_i_pct,
                    pid_d_pct: pkt.pid_d_pct,
                };
                g.records.push_back(Record {
                    received_at: now,
                    packet: pkt,
                });
                // Prune old records.
                let cutoff = now - g.retention;
                while g.records.front().is_some_and(|r| r.received_at < cutoff) {
                    g.records.pop_front();
                }
                return Some(record);
            }
        } else {
            g.latest = Some(pkt);
        }
        None
    }

    /// Latest packet, if any.
    pub fn latest(&self) -> Option<Packet> {
        self.0.read().unwrap().latest.clone()
    }

    /// Return the latest packet and collecting state in a single lock acquisition.
    pub fn latest_with_collecting(&self) -> (Option<Packet>, bool) {
        let g = self.0.read().unwrap();
        (g.latest.clone(), g.collecting)
    }

    /// Start or stop recording history into the ring buffer.
    pub fn set_collecting(&self, enabled: bool) {
        self.0.write().unwrap().collecting = enabled;
    }

    /// Return history points, total sample count, and derived sample interval in a single read-lock acquisition.
    /// Downsamples using LTTB (Largest Triangle Three Buckets) so the response always spans the full
    /// history but never exceeds `max_points`, while preserving visually salient peaks and troughs.
    /// Each point carries `gap_before` — set by inspecting raw source record timestamps so real data
    /// gaps (server offline, device reboot) are reported accurately regardless of downsampling ratio.
    /// `sample_interval_s` is derived from the median uptime gap between consecutive returned records.
    pub fn history_data(&self, max_points: usize) -> (Vec<HistoryPoint>, u32, u32) {
        let g = self.0.read().unwrap();
        let n = g.records.len();
        let total = n as u32;

        // LTTB: select source indices that best represent the data visually.
        // Always includes the first and last records; intermediate points are chosen per bucket to
        // maximise the triangle area formed with the previously-selected point and the average of
        // the next bucket.  Falls back to identity when n <= max_points.
        let source: Vec<&Record> = g.records.iter().collect();
        let indices: Vec<usize> = if n <= max_points {
            (0..n).collect()
        } else {
            lttb_indices(&source, max_points)
        };

        // Derive sample interval from the median uptime gap between consecutive returned records.
        let interval_s: u32 = if indices.len() >= 2 {
            let mut gaps: Vec<u32> = indices
                .windows(2)
                .filter_map(|w| {
                    let a = source[w[0]].packet.uptime_s;
                    let b = source[w[1]].packet.uptime_s;
                    b.checked_sub(a).filter(|&d| d > 0 && d < 86400)
                })
                .collect();
            if gaps.is_empty() {
                1
            } else {
                gaps.sort_unstable();
                gaps[gaps.len() / 2] // median
            }
        } else {
            1
        };

        // A "real gap" in the source data: any consecutive pair of raw records whose wall-clock
        // receive-time difference materially exceeds their uptime delta (records arrive ~1 s apart).
        let gap_threshold = std::time::Duration::from_secs(5);

        let points = indices
            .iter()
            .enumerate()
            .map(|(out_idx, &src_idx)| {
                let r = source[src_idx];
                let t_s = r
                    .received_at
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();

                // Scan the source records *consumed* by this output point for real time gaps.
                let gap_before = if out_idx == 0 {
                    false
                } else {
                    let prev_src = indices[out_idx - 1];
                    // Any pair of consecutive raw records between prev_src and src_idx with a gap.
                    (prev_src..src_idx).any(|k| {
                        source[k + 1]
                            .received_at
                            .duration_since(source[k].received_at)
                            .unwrap_or_default()
                            > gap_threshold
                    })
                };

                HistoryPoint {
                    seq: r.packet.seq,
                    t_s,
                    gap_before,
                    temp_c: nan_to_null(r.packet.temps[0]),
                    target_c: r.packet.target_c,
                    output_pct: r.packet.output_pct,
                    window_step: r.packet.window_step,
                    on_steps: r.packet.on_steps,
                    relay_on: r.packet.relay_on,
                    extra_temps: [
                        nan_to_null(r.packet.temps[1]),
                        nan_to_null(r.packet.temps[2]),
                    ],
                    pid_p_pct: r.packet.pid_p_pct,
                    pid_i_pct: r.packet.pid_i_pct,
                    pid_d_pct: r.packet.pid_d_pct,
                }
            })
            .collect();
        (points, total, interval_s)
    }

    pub fn clear(&self) {
        let mut g = self.0.write().unwrap();
        g.records.clear();
        g.latest = None;
        g.packets_received = 0;
        g.packets_dropped = 0;
        g.last_seq = None;
    }

    /// Serialise the ring buffer as a vec of `PersistedRecord` for file rewrite/compaction.
    pub fn current_records(&self) -> Vec<PersistedRecord> {
        let g = self.0.read().unwrap();
        g.records
            .iter()
            .map(|r| {
                let t = r
                    .received_at
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let p = &r.packet;
                PersistedRecord {
                    t,
                    seq: p.seq,
                    uptime_s: p.uptime_s,
                    temp0: nan_to_null(p.temps[0]),
                    temp1: nan_to_null(p.temps[1]),
                    temp2: nan_to_null(p.temps[2]),
                    target_c: p.target_c,
                    output_pct: p.output_pct,
                    relay_on: p.relay_on,
                    device_collecting: p.collecting,
                    ntp_synced: p.ntp_synced,
                    window_step: p.window_step,
                    on_steps: p.on_steps,
                    sensor_status: p.sensor_status,
                    device_ip: p.device_ip,
                    sensor_count: p.sensor_count,
                    pid_p_pct: p.pid_p_pct,
                    pid_i_pct: p.pid_i_pct,
                    pid_d_pct: p.pid_d_pct,
                }
            })
            .collect()
    }

    /// Restore from persisted records, dropping those older than the retention window.
    /// Returns the number of records loaded.
    pub fn restore(&self, records: Vec<PersistedRecord>) -> usize {
        let mut g = self.0.write().unwrap();
        let now = SystemTime::now();
        let cutoff = now.checked_sub(g.retention).unwrap_or(UNIX_EPOCH);
        let mut count = 0usize;
        let mut last_collecting = false;
        for r in records {
            last_collecting = r.device_collecting;
            let received_at = UNIX_EPOCH + Duration::from_secs(r.t);
            if received_at < cutoff {
                continue;
            }
            let packet = Packet {
                version: PACKET_VERSION,
                hostname: [0u8; 20],
                seq: r.seq,
                uptime_s: r.uptime_s,
                temps: [
                    r.temp0.unwrap_or(f32::NAN),
                    r.temp1.unwrap_or(f32::NAN),
                    r.temp2.unwrap_or(f32::NAN),
                ],
                target_c: r.target_c,
                output_pct: r.output_pct,
                relay_on: r.relay_on,
                collecting: r.device_collecting,
                ntp_synced: r.ntp_synced,
                history_clear: false,
                heat_on: false,
                window_step: r.window_step,
                on_steps: r.on_steps,
                sensor_status: r.sensor_status,
                device_ip: r.device_ip,
                sensor_count: r.sensor_count,
                deadband_c: 0.5,
                pid_p_pct: r.pid_p_pct,
                pid_i_pct: r.pid_i_pct,
                pid_d_pct: r.pid_d_pct,
            };
            g.records.push_back(Record {
                received_at,
                packet,
            });
            count += 1;
        }
        // Restore the latest packet so /status works immediately without waiting
        // for the first live UDP packet after a server restart.
        g.latest = g.records.back().map(|r| r.packet.clone());
        g.collecting = last_collecting;
        count
    }

    pub fn telemetry_stats(&self) -> TelemetryStats {
        let g = self.0.read().unwrap();
        let total = g.packets_received + g.packets_dropped;
        TelemetryStats {
            packets_received: g.packets_received,
            packets_dropped: g.packets_dropped,
            drop_rate_pct: if total > 0 {
                g.packets_dropped as f64 * 100.0 / total as f64
            } else {
                0.0
            },
            last_seq: g.last_seq,
        }
    }
}

/// Receiver-side telemetry statistics.
#[derive(Clone, Serialize)]
pub struct TelemetryStats {
    pub packets_received: u64,
    pub packets_dropped: u64,
    pub drop_rate_pct: f64,
    pub last_seq: Option<u32>,
}

fn nan_to_null(v: f32) -> Option<f32> {
    if v.is_nan() { None } else { Some(v) }
}

// ── Persistence types ─────────────────────────────────────────────────────────

/// One serialised history record.  `temp0/1/2` are `None` when the probe had
/// no valid reading (mapped to/from `f32::NAN` in the live `Packet`).
#[derive(Serialize, Deserialize)]
pub struct PersistedRecord {
    /// Unix timestamp (seconds) of when the packet was received.
    pub t: u64,
    pub seq: u32,
    pub uptime_s: u32,
    pub temp0: Option<f32>,
    pub temp1: Option<f32>,
    pub temp2: Option<f32>,
    pub target_c: f32,
    pub output_pct: u8,
    pub relay_on: bool,
    pub device_collecting: bool,
    pub ntp_synced: bool,
    pub window_step: u8,
    pub on_steps: u8,
    pub sensor_status: [u8; 3],
    pub device_ip: [u8; 4],
    pub sensor_count: u8,
    /// Active PID term contributions (%), 0 for historical records that pre-date v5.
    #[serde(default)]
    pub pid_p_pct: i8,
    #[serde(default)]
    pub pid_i_pct: i8,
    #[serde(default)]
    pub pid_d_pct: i8,
}

/// Matches the dashboard's expected history point array:
/// `[seq, temp_c, target_c, output_pct, window_step, on_steps, relay_on, extra1, extra2, pid_p_pct, pid_i_pct, pid_d_pct, t_s, gap_before]`
#[derive(Serialize)]
pub struct HistoryPoint {
    pub seq: u32,
    /// Wall-clock Unix timestamp (seconds) when this record was received by the server.
    pub t_s: u64,
    /// True if there is a real data gap in the source records immediately before this point
    /// (e.g. server was offline or device rebooted).  Set by inspecting raw record timestamps
    /// so it is accurate regardless of the downsampling ratio.
    pub gap_before: bool,
    pub temp_c: Option<f32>,
    pub target_c: f32,
    pub output_pct: u8,
    pub window_step: u8,
    pub on_steps: u8,
    pub relay_on: bool,
    pub extra_temps: [Option<f32>; 2],
    pub pid_p_pct: i8,
    pub pid_i_pct: i8,
    pub pid_d_pct: i8,
}

// ── LTTB downsampling ─────────────────────────────────────────────────────────

/// Largest-Triangle-Three-Buckets downsampling.
///
/// Returns the selected source indices (always includes index 0 and n-1).
/// Uses `uptime_s` as the x-axis and `temps[0]` (primary probe) as the y-axis.
/// NaN temperatures are treated as 0.0 for the area calculation only.
fn lttb_indices(records: &[&Record], threshold: usize) -> Vec<usize> {
    let n = records.len();
    if n <= threshold {
        return (0..n).collect();
    }

    let temp = |i: usize| -> f64 {
        let t = records[i].packet.temps[0];
        if t.is_nan() { 0.0 } else { t as f64 }
    };
    let time = |i: usize| -> f64 { records[i].packet.uptime_s as f64 };

    let mut selected: Vec<usize> = Vec::with_capacity(threshold);
    selected.push(0);

    let nbuckets = threshold - 2; // intermediate buckets
    let mut a = 0usize; // source index of previously selected point

    for i in 0..nbuckets {
        // Current bucket: integer-arithmetic bucket boundaries to avoid float drift.
        let curr_start = i * (n - 2) / nbuckets + 1;
        let curr_end = (i + 1) * (n - 2) / nbuckets + 1;

        // Next bucket (look-ahead for the average point C).
        let next_start = curr_end;
        let next_end = if i + 1 < nbuckets {
            (i + 2) * (n - 2) / nbuckets + 1
        } else {
            n - 1 // last point is always included separately
        };

        // Average of next bucket (point C).
        let next_len = (next_end - next_start).max(1) as f64;
        let avg_x: f64 = (next_start..next_end).map(time).sum::<f64>() / next_len;
        let avg_y: f64 = (next_start..next_end).map(temp).sum::<f64>() / next_len;

        // Point A: previously selected.
        let ax = time(a);
        let ay = temp(a);

        // Find point B in current bucket with the largest triangle area (A, B, C).
        let mut max_area = -1.0f64;
        let mut max_j = curr_start;
        for j in curr_start..curr_end {
            let bx = time(j);
            let by = temp(j);
            // Twice the triangle area = |det| — we skip the ×0.5 since we only compare.
            let area = ((ax - avg_x) * (by - ay) - (ax - bx) * (avg_y - ay)).abs();
            if area > max_area {
                max_area = area;
                max_j = j;
            }
        }
        selected.push(max_j);
        a = max_j;
    }

    selected.push(n - 1);
    selected
}
