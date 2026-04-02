// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

//! Metrics serialization and formatting.
//!
//! This module provides serializers for the device state in multiple formats:
//! - JSON for dashboard consumption
//! - Text for human debugging
//! - Prometheus for metric collection

use alloc::string::String;
use core::fmt::Write as _;

use super::{shared, status};
use crate::{PID_KD, PID_KI, PID_KP, device_hostname};

fn ntp_peer_snapshots() -> heapless::Vec<NtpPeerSnapshot, NTP_MAX_TRACKED_PEERS> {
    status::ntp_peers_snapshot()
}
const JSON_STATUS_CAPACITY: usize = 3072;
const TEXT_STATUS_CAPACITY: usize = 192;
const PROM_STATUS_CAPACITY: usize = 8192;
const NTP_MAX_TRACKED_PEERS: usize = shared::NTP_MAX_CONFIG_SERVERS + 1;

use status::NtpPeerSnapshot;

fn write_ipv4(out: &mut String, octets: [u8; 4]) {
    let _ = write!(
        out,
        "{}.{}.{}.{}",
        octets[0], octets[1], octets[2], octets[3]
    );
}

fn append_json_quoted_ipv4(out: &mut String, octets: [u8; 4]) {
    out.push('"');
    write_ipv4(out, octets);
    out.push('"');
}

fn append_json_optional_u32(out: &mut String, value: Option<u32>) {
    if let Some(value) = value {
        let _ = write!(out, "{}", value);
    } else {
        out.push_str("null");
    }
}

fn append_json_optional_ms3_from_us(out: &mut String, value_us: Option<u32>) {
    if let Some(value_us) = value_us {
        let whole = value_us / 1_000;
        let frac = value_us % 1_000;
        let _ = write!(out, "{}.{:03}", whole, frac);
    } else {
        out.push_str("null");
    }
}

fn append_json_optional_ms3_from_signed_us(out: &mut String, value_us: Option<i32>) {
    if let Some(value_us) = value_us {
        let sign = if value_us < 0 { "-" } else { "" };
        let abs_us = value_us.unsigned_abs();
        let whole = abs_us / 1_000;
        let frac = abs_us % 1_000;
        let _ = write!(out, "{}{}.{:03}", sign, whole, frac);
    } else {
        out.push_str("null");
    }
}

fn append_ntp_peer_json(
    out: &mut String,
    peer: NtpPeerSnapshot,
    selected_source: shared::NtpSource,
    selected_address: [u8; 4],
) {
    let is_selected = selected_source == peer.source && selected_address == peer.address;
    out.push_str("\n        {\n          \"address\": ");
    append_json_quoted_ipv4(out, peer.address);
    let _ = write!(
        out,
        concat!(
            ",\n",
            "          \"source\": \"{}\",\n",
            "          \"selected\": {},\n",
            "          \"stratum\": ",
        ),
        peer.source.label(),
        if is_selected { "true" } else { "false" },
    );
    append_json_optional_u32(out, peer.has_sample.then_some(peer.stratum as u32));
    out.push_str(",\n          \"latency_ms\": ");
    append_json_optional_ms3_from_us(out, peer.has_sample.then_some(peer.latency_us));
    out.push_str(",\n          \"jitter_ms\": ");
    append_json_optional_ms3_from_us(out, peer.has_sample.then_some(peer.jitter_us));
    out.push_str(",\n          \"offset_ms\": ");
    append_json_optional_ms3_from_signed_us(out, peer.offset_us);
    out.push_str(",\n          \"offset_jitter_ms\": ");
    append_json_optional_ms3_from_us(out, peer.offset_jitter_us);
    let _ = write!(
        out,
        concat!(
            ",\n",
            "          \"success_count\": {},\n",
            "          \"fail_count\": {},\n",
            "          \"last_sync_uptime_s\": ",
        ),
        peer.success_count, peer.fail_count,
    );
    append_json_optional_u32(
        out,
        (peer.success_count > 0).then_some(peer.last_sync_uptime_s),
    );
    out.push_str("\n        }");
}

/// Serialize all device state as JSON.
#[allow(
    clippy::large_stack_frames,
    reason = "JSON formatting uses large local buffers in this no_std telemetry path"
)]
pub fn json() -> String {
    let snapshot = status::metrics_snapshot();
    let temp_centi = snapshot.temp_centi;
    let pid_deci = snapshot.pid_deci;
    let relay_on = snapshot.relay_on;
    let collection_enabled = snapshot.collection_enabled;
    let sensor_status_code = snapshot.sensor_status_code;
    let led_red = snapshot.led_red;
    let led_green = snapshot.led_green;
    let led_blue = snapshot.led_blue;
    let pid_window_step = snapshot.pid_window_step;
    let pid_on_steps = snapshot.pid_on_steps;
    let target_c = snapshot.target_c;
    let target_f = snapshot.target_f;
    let ip_valid = snapshot.ip_valid;
    let net_state_code = snapshot.net_state_code;
    let ip_octets = snapshot.ip_octets;
    let ntp_synced = snapshot.ntp_synced;
    let ntp_sync_count = snapshot.ntp_sync_count;
    let ntp_source_code = snapshot.ntp_source_code;
    let ntp_uptime_at_sync = snapshot.ntp_uptime_at_sync;
    let current_ntp_time = snapshot.current_ntp_time;
    let master_ip = snapshot.master_ip;
    let probe_name = snapshot.probe_name;

    let ntp_source = shared::NtpSource::from_u8(ntp_source_code);
    let net_state = status::NetState::from_u8(net_state_code);

    let uptime_s = embassy_time::Instant::now().as_ticks() / embassy_time::TICK_HZ;
    let heap_free = esp_alloc::HEAP.free();
    let heap_used = esp_alloc::HEAP.used();
    let heap_stats = esp_alloc::HEAP.stats();

    let peer_snapshots = ntp_peer_snapshots();
    let selected_peer = peer_snapshots
        .iter()
        .copied()
        .find(|peer| peer.source == ntp_source && peer.address == master_ip);
    let ntp_master_has_sample = selected_peer.is_some_and(|p| p.has_sample);
    let ntp_master_stratum = selected_peer.map(|p| p.stratum).unwrap_or(0);
    let ntp_master_latency_us = selected_peer.map(|p| p.latency_us).unwrap_or(0);
    let ntp_master_jitter_us = selected_peer.map(|p| p.jitter_us).unwrap_or(0);
    let selected_master_success_count = selected_peer.map(|peer| peer.success_count);
    let selected_master_fail_count = selected_peer.map(|peer| peer.fail_count);
    let selected_master_offset_us = selected_peer.and_then(|peer| peer.offset_us);
    let selected_master_offset_jitter_us = selected_peer.and_then(|peer| peer.offset_jitter_us);

    const UNKNOWN_TEMPERATURE_CENTI: i32 = i32::MIN;
    let temp_cf = if temp_centi == UNKNOWN_TEMPERATURE_CENTI {
        None
    } else {
        let c = temp_centi as f32 / 100.0;
        let f = c * 9.0 / 5.0 + 32.0;
        Some((c, f))
    };

    let mut out = String::with_capacity(JSON_STATUS_CAPACITY);
    let _ = write!(
        out,
        concat!(
            "{{\n",
            "  \"device\": \"{}\",\n",
            "  \"sensor\": {{\n",
            "    \"ds18b20\": {{\n",
            "      \"name\": \"{}\",\n",
            "      \"temperature_c\": ",
        ),
        device_hostname(),
        probe_name,
    );
    if let Some((temp_c, _)) = temp_cf {
        let _ = write!(out, "{:.2}", temp_c);
    } else {
        out.push_str("null");
    }
    out.push_str(",\n      \"temperature_f\": ");
    if let Some((_, temp_f)) = temp_cf {
        let _ = write!(out, "{:.2}", temp_f);
    } else {
        out.push_str("null");
    }
    let _ = write!(
        out,
        concat!(
            ",\n",
            "      \"error\": \"{}\"\n",
            "    }}\n",
            "  }},\n",
            "  \"pid\": {{\n",
            "    \"target_c\": {:.1},\n",
            "    \"target_f\": {:.1},\n",
            "    \"kp\": {:.2},\n",
            "    \"ki\": {:.2},\n",
            "    \"kd\": {:.2},\n",
            "    \"output_percent\": {:.1},\n",
            "    \"window_step\": {},\n",
            "    \"on_steps\": {},\n",
            "    \"relay_on\": {}\n",
            "  }},\n",
            "  \"led\": {{\n",
            "    \"r\": {},\n",
            "    \"g\": {},\n",
            "    \"b\": {}\n",
            "  }},\n",
            "  \"system\": {{\n",
            "    \"ip\": ",
        ),
        status::sensor_status_label(sensor_status_code),
        target_c,
        target_f,
        PID_KP,
        PID_KI,
        PID_KD,
        pid_deci as f32 / 10.0,
        pid_window_step,
        pid_on_steps,
        if relay_on { "true" } else { "false" },
        led_red,
        led_green,
        led_blue,
    );
    if ip_valid {
        append_json_quoted_ipv4(&mut out, ip_octets);
    } else {
        let label = match net_state {
            status::NetState::LinkDown => "Error(link_down)",
            status::NetState::DhcpPending => "Error(dhcp_pending)",
            _ => "Error",
        };
        let _ = write!(out, "\"{}\"", label);
    }

    let _ = write!(
        out,
        concat!(
            ",\n",
            "    \"collecting\": {},\n",
            "    \"ntp\": {{\n",
            "      \"synced\": {},\n",
            "      \"time\": ",
        ),
        if collection_enabled { "true" } else { "false" },
        if ntp_synced { "true" } else { "false" },
    );
    if let Some(secs) = current_ntp_time {
        let _ = write!(out, "\"{}\"", shared::unix_to_iso8601(secs));
    } else {
        out.push_str("null");
    }
    out.push_str(",\n      \"master_address\": ");
    if ntp_synced {
        append_json_quoted_ipv4(&mut out, master_ip);
    } else {
        out.push_str("null");
    }
    out.push_str(",\n      \"master_source\": ");
    if ntp_synced {
        let _ = write!(out, "\"{}\"", ntp_source.label());
    } else {
        out.push_str("null");
    }
    let _ = write!(
        out,
        concat!(
            ",\n",
            "      \"sync_count\": {},\n",
            "      \"last_sync_uptime_s\": ",
        ),
        ntp_sync_count,
    );
    append_json_optional_u32(&mut out, ntp_synced.then_some(ntp_uptime_at_sync));
    out.push_str(",\n      \"master_stratum\": ");
    append_json_optional_u32(
        &mut out,
        (ntp_synced && ntp_master_has_sample).then_some(ntp_master_stratum as u32),
    );
    out.push_str(",\n      \"master_latency_ms\": ");
    append_json_optional_ms3_from_us(
        &mut out,
        (ntp_synced && ntp_master_has_sample).then_some(ntp_master_latency_us),
    );
    out.push_str(",\n      \"master_jitter_ms\": ");
    append_json_optional_ms3_from_us(
        &mut out,
        (ntp_synced && ntp_master_has_sample).then_some(ntp_master_jitter_us),
    );
    out.push_str(",\n      \"master_offset_ms\": ");
    append_json_optional_ms3_from_signed_us(
        &mut out,
        if ntp_synced {
            selected_master_offset_us
        } else {
            None
        },
    );
    out.push_str(",\n      \"master_offset_jitter_ms\": ");
    append_json_optional_ms3_from_us(
        &mut out,
        if ntp_synced {
            selected_master_offset_jitter_us
        } else {
            None
        },
    );
    out.push_str(",\n      \"master_success_count\": ");
    append_json_optional_u32(
        &mut out,
        if ntp_synced {
            selected_master_success_count
        } else {
            None
        },
    );
    out.push_str(",\n      \"master_fail_count\": ");
    append_json_optional_u32(
        &mut out,
        if ntp_synced {
            selected_master_fail_count
        } else {
            None
        },
    );
    out.push_str(",\n      \"peers\": [");

    let mut first_peer = true;
    for peer in peer_snapshots {
        if !first_peer {
            out.push(',');
        }
        append_ntp_peer_json(&mut out, peer, ntp_source, master_ip);
        first_peer = false;
    }

    out.push_str("\n      ]\n");
    out.push_str("    },\n");
    let _ = writeln!(out, "    \"uptime_s\": {},", uptime_s);
    let _ = writeln!(out, "    \"heap_free_bytes\": {},", heap_free);
    let _ = writeln!(out, "    \"heap_used_bytes\": {},", heap_used);
    out.push_str("    \"heap\": {\n");
    let _ = writeln!(out, "      \"size_bytes\": {},", heap_stats.size);
    let _ = writeln!(
        out,
        "      \"current_usage_bytes\": {},",
        heap_stats.current_usage,
    );
    out.push_str("      \"max_usage_bytes\": null");
    out.push_str(",\n      \"total_freed_bytes\": null");
    out.push_str(",\n      \"total_allocated_bytes\": null");
    out.push_str(",\n      \"memory_layout\": [");

    let mut first_region = true;
    for region in heap_stats.region_stats.iter().flatten() {
        if !first_region {
            out.push(',');
        }
        let region_type = if region
            .capabilities
            .contains(esp_alloc::MemoryCapability::Internal)
        {
            "internal"
        } else if region
            .capabilities
            .contains(esp_alloc::MemoryCapability::External)
        {
            "external"
        } else {
            "unknown"
        };
        let used_percent = if region.size == 0 {
            0
        } else {
            region.used.saturating_mul(100) / region.size
        };
        let used_blocks = used_percent.saturating_mul(10) / 100;
        out.push_str("\n        {\"type\":\"");
        out.push_str(region_type);
        let _ = write!(
            out,
            "\",\"size_bytes\":{},\"used_bytes\":{},\"free_bytes\":{},\"used_percent\":{},\"bar\":\"",
            region.size, region.used, region.free, used_percent,
        );
        for _ in 0..used_blocks {
            out.push('#');
        }
        for _ in used_blocks..10 {
            out.push('-');
        }
        out.push_str("\"}");
        first_region = false;
    }
    if !first_region {
        out.push('\n');
        out.push_str("      ");
    }
    out.push_str("]\n");
    out.push_str("    }\n");
    out.push_str("  }\n");
    out.push_str("}\n");
    out
}

pub fn history_json(max_points: usize) -> String {
    let points = status::history_snapshot(max_points);
    let mut out = String::with_capacity(256 + points.len().saturating_mul(40));
    let _ = write!(
        out,
        concat!(
            "{{\n",
            "  \"sample_interval_s\": {},\n",
            "  \"total_samples\": {},\n",
            "  \"points\": ["
        ),
        status::history_sample_interval_secs(),
        status::history_total_samples(),
    );

    for (idx, point) in points.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        let _ = write!(
            out,
            "\n    [{},{:.2},{:.2},{:.1},{},{},{}]",
            point.seq,
            point.temp_c,
            point.target_c,
            point.output_percent,
            point.window_step,
            point.on_steps,
            if point.relay_on { 1 } else { 0 },
        );
    }

    out.push_str("\n  ]\n}\n");
    out
}

/// Serialize device state as human-readable text.
#[allow(
    clippy::large_stack_frames,
    reason = "status text formatting builds multi-argument fmt state on stack"
)]
pub fn text() -> String {
    let snapshot = status::metrics_snapshot();
    let temp_centi = snapshot.temp_centi;
    let pid_deci = snapshot.pid_deci;
    let relay_on = snapshot.relay_on;
    let sensor_status_code = snapshot.sensor_status_code;
    let led_red = snapshot.led_red;
    let led_green = snapshot.led_green;
    let led_blue = snapshot.led_blue;
    let target_c = snapshot.target_c;
    let target_f = snapshot.target_f;
    let ip_valid = snapshot.ip_valid;
    let net_state_code = snapshot.net_state_code;
    let ip_octets = snapshot.ip_octets;
    let probe_name = snapshot.probe_name;

    let net_state = status::NetState::from_u8(net_state_code);

    let mut out = String::with_capacity(TEXT_STATUS_CAPACITY);
    out.push_str("status: ip=");
    if ip_valid {
        write_ipv4(&mut out, ip_octets);
    } else {
        out.push_str(match net_state {
            status::NetState::LinkDown => "Error(link_down)",
            status::NetState::DhcpPending => "Error(dhcp_pending)",
            _ => "Error",
        });
    }

    const UNKNOWN_TEMPERATURE_CENTI: i32 = i32::MIN;
    if temp_centi == UNKNOWN_TEMPERATURE_CENTI {
        let _ = write!(
            out,
            " probe={} sensor={} target={:.1}C/{:.1}F pid={:.1}% relay={} led=({}, {}, {}) heap_free={}B",
            probe_name,
            status::sensor_status_label(sensor_status_code),
            target_c,
            target_f,
            pid_deci as f32 / 10.0,
            if relay_on { "on" } else { "off" },
            led_red,
            led_green,
            led_blue,
            esp_alloc::HEAP.free(),
        );
    } else {
        let temp_c = temp_centi as f32 / 100.0;
        let temp_f = temp_c * 9.0 / 5.0 + 32.0;
        let uptime_s = embassy_time::Instant::now().as_ticks() / embassy_time::TICK_HZ;
        let _ = write!(
            out,
            " probe={} temp={:.2}C/{:.2}F target={:.1}C/{:.1}F sensor={} pid={:.1}% relay={} led=({}, {}, {}) uptime={}s heap_free={}B",
            probe_name,
            temp_c,
            temp_f,
            target_c,
            target_f,
            status::sensor_status_label(sensor_status_code),
            pid_deci as f32 / 10.0,
            if relay_on { "on" } else { "off" },
            led_red,
            led_green,
            led_blue,
            uptime_s,
            esp_alloc::HEAP.free(),
        );
    }

    out
}

/// Serialize device state as Prometheus metrics.
#[allow(
    clippy::large_stack_frames,
    reason = "Prometheus serialization accumulates many labels and temporary values"
)]
pub fn prometheus() -> String {
    let snapshot = status::prometheus_snapshot();
    let temp_centi = snapshot.temp_centi;
    let pid_deci = snapshot.pid_deci;
    let pid_window_step = snapshot.pid_window_step;
    let pid_on_steps = snapshot.pid_on_steps;
    let relay_on = snapshot.relay_on;
    let target_c = snapshot.target_c;
    let target_f = snapshot.target_f;
    let ntp_synced = snapshot.ntp_synced;
    let ntp_sync_count = snapshot.ntp_sync_count;
    let ntp_source_code = snapshot.ntp_source_code;
    let ntp_uptime_at_sync = snapshot.ntp_uptime_at_sync;
    let master_ip = snapshot.master_ip;
    let probe_name = snapshot.probe_name;

    let ntp_source = shared::NtpSource::from_u8(ntp_source_code);

    let uptime_s = embassy_time::Instant::now().as_ticks() / embassy_time::TICK_HZ;
    let heap_free = esp_alloc::HEAP.free();
    let heap_used = esp_alloc::HEAP.used();
    let heap_stats = esp_alloc::HEAP.stats();

    let peer_snapshots = ntp_peer_snapshots();
    let selected_peer = peer_snapshots
        .iter()
        .copied()
        .find(|peer| peer.source == ntp_source && peer.address == master_ip);
    let ntp_master_has_sample = selected_peer.is_some_and(|p| p.has_sample);
    let ntp_master_stratum = selected_peer.map(|p| p.stratum).unwrap_or(0);
    let ntp_master_latency_us = selected_peer.map(|p| p.latency_us).unwrap_or(0);
    let ntp_master_jitter_us = selected_peer.map(|p| p.jitter_us).unwrap_or(0);
    let selected_master_success_count = selected_peer.map(|peer| peer.success_count);
    let selected_master_fail_count = selected_peer.map(|peer| peer.fail_count);
    let selected_master_offset_jitter_us = selected_peer.and_then(|peer| peer.offset_jitter_us);
    let selected_master_offset_us = selected_peer.and_then(|peer| peer.offset_us);

    let mut out = String::with_capacity(PROM_STATUS_CAPACITY);
    out.push_str("# HELP brewster_up Firmware heartbeat metric.\n");
    out.push_str("# TYPE brewster_up gauge\n");
    out.push_str("brewster_up 1\n");

    out.push_str("# HELP brewster_uptime_seconds Device uptime in seconds.\n");
    out.push_str("# TYPE brewster_uptime_seconds gauge\n");
    let _ = writeln!(out, "brewster_uptime_seconds {}", uptime_s);

    out.push_str("# HELP brewster_heap_free_bytes Free heap in bytes.\n");
    out.push_str("# TYPE brewster_heap_free_bytes gauge\n");
    let _ = writeln!(out, "brewster_heap_free_bytes {}", heap_free);

    out.push_str("# HELP brewster_heap_used_bytes Used heap in bytes.\n# TYPE brewster_heap_used_bytes gauge\n");
    let _ = writeln!(out, "brewster_heap_used_bytes {}", heap_used);

    out.push_str("# HELP brewster_heap_size_bytes Total configured heap size in bytes.\n# TYPE brewster_heap_size_bytes gauge\n");
    let _ = writeln!(out, "brewster_heap_size_bytes {}", heap_stats.size);

    out.push_str(
        "# HELP brewster_heap_current_usage_bytes Current heap usage in bytes (aggregated).\n",
    );
    out.push_str("# TYPE brewster_heap_current_usage_bytes gauge\n");
    let _ = writeln!(
        out,
        "brewster_heap_current_usage_bytes {}",
        heap_stats.current_usage
    );

    out.push_str(
        "# HELP brewster_heap_max_usage_bytes Peak heap usage in bytes (NaN if unavailable).\n",
    );
    out.push_str("# TYPE brewster_heap_max_usage_bytes gauge\n");
    out.push_str("brewster_heap_max_usage_bytes NaN\n");

    out.push_str("# HELP brewster_heap_total_freed_bytes Total freed heap bytes since boot (NaN if unavailable).\n# TYPE brewster_heap_total_freed_bytes gauge\n");
    out.push_str("brewster_heap_total_freed_bytes NaN\n");

    out.push_str("# HELP brewster_heap_total_allocated_bytes Total allocated heap bytes since boot (NaN if unavailable).\n# TYPE brewster_heap_total_allocated_bytes gauge\n");
    out.push_str("brewster_heap_total_allocated_bytes NaN\n");

    out.push_str("# HELP brewster_heap_region_size_bytes Heap region size in bytes.\n");
    out.push_str("# TYPE brewster_heap_region_size_bytes gauge\n");
    out.push_str("# HELP brewster_heap_region_used_bytes Heap region used bytes.\n");
    out.push_str("# TYPE brewster_heap_region_used_bytes gauge\n");
    out.push_str("# HELP brewster_heap_region_free_bytes Heap region free bytes.\n");
    out.push_str("# TYPE brewster_heap_region_free_bytes gauge\n");
    out.push_str("# HELP brewster_heap_region_used_percent Heap region used percent.\n");
    out.push_str("# TYPE brewster_heap_region_used_percent gauge\n");
    for (idx, region) in heap_stats.region_stats.iter().enumerate() {
        if let Some(region) = region {
            let region_type = if region
                .capabilities
                .contains(esp_alloc::MemoryCapability::Internal)
            {
                "internal"
            } else if region
                .capabilities
                .contains(esp_alloc::MemoryCapability::External)
            {
                "external"
            } else {
                "unknown"
            };
            let used_percent = if region.size == 0 {
                0
            } else {
                region.used.saturating_mul(100) / region.size
            };
            let _ = writeln!(
                out,
                "brewster_heap_region_size_bytes{{region=\"{}\",type=\"{}\"}} {}",
                idx, region_type, region.size
            );
            let _ = writeln!(
                out,
                "brewster_heap_region_used_bytes{{region=\"{}\",type=\"{}\"}} {}",
                idx, region_type, region.used
            );
            let _ = writeln!(
                out,
                "brewster_heap_region_free_bytes{{region=\"{}\",type=\"{}\"}} {}",
                idx, region_type, region.free
            );
            let _ = writeln!(
                out,
                "brewster_heap_region_used_percent{{region=\"{}\",type=\"{}\"}} {}",
                idx, region_type, used_percent
            );
        }
    }

    out.push_str(
        "# HELP brewster_sensor_temperature_valid 1 when a valid sensor reading is present.\n",
    );
    out.push_str("# TYPE brewster_sensor_temperature_valid gauge\n");
    const UNKNOWN_TEMPERATURE_CENTI: i32 = i32::MIN;
    let has_temp = temp_centi != UNKNOWN_TEMPERATURE_CENTI;
    let _ = writeln!(
        out,
        "brewster_sensor_temperature_valid {}",
        if has_temp { 1 } else { 0 }
    );

    out.push_str("# HELP brewster_sensor_info Sensor metadata including configured probe name.\n");
    out.push_str("# TYPE brewster_sensor_info gauge\n");
    let _ = writeln!(out, "brewster_sensor_info{{name=\"{}\"}} 1", probe_name);

    let temp_cf_opt = if has_temp {
        let temp_c = temp_centi as f32 / 100.0;
        let temp_f = temp_c * 9.0 / 5.0 + 32.0;
        Some((temp_c, temp_f))
    } else {
        None
    };

    out.push_str("# HELP brewster_sensor_temperature_celsius Sensor temperature in Celsius.\n");
    out.push_str("# TYPE brewster_sensor_temperature_celsius gauge\n");
    match temp_cf_opt {
        Some((temp_c, _)) => {
            let _ = writeln!(out, "brewster_sensor_temperature_celsius {:.2}", temp_c);
        }
        None => {
            out.push_str("brewster_sensor_temperature_celsius NaN\n");
        }
    }

    out.push_str(
        "# HELP brewster_sensor_temperature_fahrenheit Sensor temperature in Fahrenheit.\n",
    );
    out.push_str("# TYPE brewster_sensor_temperature_fahrenheit gauge\n");
    match temp_cf_opt {
        Some((_, temp_f)) => {
            let _ = writeln!(out, "brewster_sensor_temperature_fahrenheit {:.2}", temp_f);
        }
        None => {
            out.push_str("brewster_sensor_temperature_fahrenheit NaN\n");
        }
    }

    out.push_str("# HELP brewster_temperature_c Alias for sensor temperature in Celsius.\n");
    out.push_str("# TYPE brewster_temperature_c gauge\n");
    match temp_cf_opt {
        Some((temp_c, _)) => {
            let _ = writeln!(out, "brewster_temperature_c {:.2}", temp_c);
        }
        None => {
            out.push_str("brewster_temperature_c NaN\n");
        }
    }

    out.push_str("# HELP brewster_temperature_f Alias for sensor temperature in Fahrenheit.\n");
    out.push_str("# TYPE brewster_temperature_f gauge\n");
    match temp_cf_opt {
        Some((_, temp_f)) => {
            let _ = writeln!(out, "brewster_temperature_f {:.2}", temp_f);
        }
        None => {
            out.push_str("brewster_temperature_f NaN\n");
        }
    }

    out.push_str("# HELP brewster_pid_target_celsius PID target setpoint in Celsius.\n");
    out.push_str("# TYPE brewster_pid_target_celsius gauge\n");
    let _ = writeln!(out, "brewster_pid_target_celsius {:.2}", target_c);

    out.push_str("# HELP brewster_pid_target_fahrenheit PID target setpoint in Fahrenheit.\n");
    out.push_str("# TYPE brewster_pid_target_fahrenheit gauge\n");
    let _ = writeln!(out, "brewster_pid_target_fahrenheit {:.2}", target_f);

    out.push_str("# HELP brewster_target_temperature_c Alias for PID target in Celsius.\n");
    out.push_str("# TYPE brewster_target_temperature_c gauge\n");
    let _ = writeln!(out, "brewster_target_temperature_c {:.2}", target_c);

    out.push_str("# HELP brewster_target_temperature_f Alias for PID target in Fahrenheit.\n");
    out.push_str("# TYPE brewster_target_temperature_f gauge\n");
    let _ = writeln!(out, "brewster_target_temperature_f {:.2}", target_f);

    out.push_str("# HELP brewster_pid_kp PID proportional gain.\n# TYPE brewster_pid_kp gauge\n");
    let _ = writeln!(out, "brewster_pid_kp {:.4}", PID_KP);

    out.push_str("# HELP brewster_pid_ki PID integral gain.\n# TYPE brewster_pid_ki gauge\n");
    let _ = writeln!(out, "brewster_pid_ki {:.4}", PID_KI);

    out.push_str("# HELP brewster_pid_kd PID derivative gain.\n# TYPE brewster_pid_kd gauge\n");
    let _ = writeln!(out, "brewster_pid_kd {:.4}", PID_KD);

    out.push_str("# HELP brewster_pid_output_percent PID output duty cycle percent.\n");
    out.push_str("# TYPE brewster_pid_output_percent gauge\n");
    let _ = writeln!(
        out,
        "brewster_pid_output_percent {:.1}",
        pid_deci as f32 / 10.0
    );

    out.push_str("# HELP brewster_pid_window_step Current PID window step index.\n");
    out.push_str("# TYPE brewster_pid_window_step gauge\n");
    let _ = writeln!(out, "brewster_pid_window_step {}", pid_window_step);

    out.push_str("# HELP brewster_pid_on_steps Current PID on-steps in window.\n");
    out.push_str("# TYPE brewster_pid_on_steps gauge\n");
    let _ = writeln!(out, "brewster_pid_on_steps {}", pid_on_steps);

    out.push_str(
        "# HELP brewster_relay_on 1 when heater relay is on.\n# TYPE brewster_relay_on gauge\n",
    );
    let _ = writeln!(out, "brewster_relay_on {}", if relay_on { 1 } else { 0 });

    out.push_str("# HELP brewster_ntp_synced 1 when NTP time is currently synchronized.\n# TYPE brewster_ntp_synced gauge\n");
    let _ = writeln!(
        out,
        "brewster_ntp_synced {}",
        if ntp_synced { 1 } else { 0 }
    );

    out.push_str("# HELP brewster_ntp_sync_total Number of successful NTP sync events.\n");
    out.push_str("# TYPE brewster_ntp_sync_total counter\n");
    let _ = writeln!(out, "brewster_ntp_sync_total {}", ntp_sync_count);

    out.push_str("# HELP brewster_ntp_last_sync_uptime_seconds Uptime in seconds when the current NTP anchor was recorded.\n");
    out.push_str("# TYPE brewster_ntp_last_sync_uptime_seconds gauge\n");
    if ntp_synced {
        let _ = writeln!(
            out,
            "brewster_ntp_last_sync_uptime_seconds {}",
            ntp_uptime_at_sync
        );
    }

    out.push_str("# HELP brewster_ntp_master_info Selected NTP master identity.\n");
    out.push_str("# TYPE brewster_ntp_master_info gauge\n");
    if ntp_synced {
        let _ = writeln!(
            out,
            "brewster_ntp_master_info{{source=\"{}\",address=\"{}.{}.{}.{}\"}} 1",
            ntp_source.label(),
            master_ip[0],
            master_ip[1],
            master_ip[2],
            master_ip[3]
        );
    }

    out.push_str("# HELP brewster_ntp_master_stratum Current selected NTP stratum.\n");
    out.push_str("# TYPE brewster_ntp_master_stratum gauge\n");
    if ntp_synced && ntp_master_has_sample {
        let _ = writeln!(out, "brewster_ntp_master_stratum {}", ntp_master_stratum);
    }

    out.push_str(
        "# HELP brewster_ntp_master_latency_seconds Current selected NTP RTT estimate in seconds.\n",
    );
    out.push_str("# TYPE brewster_ntp_master_latency_seconds gauge\n");
    if ntp_synced && ntp_master_has_sample {
        let _ = writeln!(
            out,
            "brewster_ntp_master_latency_seconds {:.6}",
            ntp_master_latency_us as f64 / 1_000_000.0
        );
    }
    out.push_str(
        "# HELP brewster_ntp_master_latency_ms Current selected NTP RTT estimate in milliseconds.\n",
    );
    out.push_str("# TYPE brewster_ntp_master_latency_ms gauge\n");
    if ntp_synced && ntp_master_has_sample {
        let _ = writeln!(
            out,
            "brewster_ntp_master_latency_ms {:.3}",
            ntp_master_latency_us as f64 / 1_000.0
        );
    }

    out.push_str("# HELP brewster_ntp_master_jitter_seconds Current selected NTP jitter estimate in seconds.\n");
    out.push_str("# TYPE brewster_ntp_master_jitter_seconds gauge\n");
    if ntp_synced && ntp_master_has_sample {
        let _ = writeln!(
            out,
            "brewster_ntp_master_jitter_seconds {:.6}",
            ntp_master_jitter_us as f64 / 1_000_000.0
        );
    }
    out.push_str("# HELP brewster_ntp_master_jitter_ms Current selected NTP jitter estimate in milliseconds.\n");
    out.push_str("# TYPE brewster_ntp_master_jitter_ms gauge\n");
    if ntp_synced && ntp_master_has_sample {
        let _ = writeln!(
            out,
            "brewster_ntp_master_jitter_ms {:.3}",
            ntp_master_jitter_us as f64 / 1_000.0
        );
    }

    out.push_str(
        "# HELP brewster_ntp_master_offset_seconds Current selected NTP offset in seconds.\n",
    );
    out.push_str("# TYPE brewster_ntp_master_offset_seconds gauge\n");
    if let Some(offset_us) = selected_master_offset_us {
        let _ = writeln!(
            out,
            "brewster_ntp_master_offset_seconds {:.6}",
            offset_us as f64 / 1_000_000.0
        );
    }
    out.push_str(
        "# HELP brewster_ntp_master_offset_ms Current selected NTP offset in milliseconds.\n",
    );
    out.push_str("# TYPE brewster_ntp_master_offset_ms gauge\n");
    if let Some(offset_us) = selected_master_offset_us {
        let _ = writeln!(
            out,
            "brewster_ntp_master_offset_ms {:.3}",
            offset_us as f64 / 1_000.0
        );
    }

    out.push_str(
        "# HELP brewster_ntp_master_offset_jitter_seconds Current selected NTP offset jitter in seconds.\n",
    );
    out.push_str("# TYPE brewster_ntp_master_offset_jitter_seconds gauge\n");
    if let Some(offset_jitter_us) = selected_master_offset_jitter_us {
        let _ = writeln!(
            out,
            "brewster_ntp_master_offset_jitter_seconds {:.6}",
            offset_jitter_us as f64 / 1_000_000.0
        );
    }
    out.push_str(
        "# HELP brewster_ntp_master_offset_jitter_ms Current selected NTP offset jitter in milliseconds.\n",
    );
    out.push_str("# TYPE brewster_ntp_master_offset_jitter_ms gauge\n");
    if let Some(offset_jitter_us) = selected_master_offset_jitter_us {
        let _ = writeln!(
            out,
            "brewster_ntp_master_offset_jitter_ms {:.3}",
            offset_jitter_us as f64 / 1_000.0
        );
    }

    out.push_str("# HELP brewster_ntp_master_success_total Success count for the currently selected NTP master peer.\n");
    out.push_str("# TYPE brewster_ntp_master_success_total gauge\n");
    if let Some(success_count) = selected_master_success_count {
        let _ = writeln!(out, "brewster_ntp_master_success_total {}", success_count);
    }

    out.push_str("# HELP brewster_ntp_master_fail_total Failure count for the currently selected NTP master peer.\n");
    out.push_str("# TYPE brewster_ntp_master_fail_total gauge\n");
    if let Some(fail_count) = selected_master_fail_count {
        let _ = writeln!(out, "brewster_ntp_master_fail_total {}", fail_count);
    }

    out.push_str("# HELP brewster_ntp_peer_success_total NTP successes per peer.\n");
    out.push_str("# TYPE brewster_ntp_peer_success_total counter\n");
    out.push_str("# HELP brewster_ntp_peer_fail_total NTP failures per peer.\n");
    out.push_str("# TYPE brewster_ntp_peer_fail_total counter\n");
    out.push_str("# HELP brewster_ntp_peer_latency_seconds Latest latency per peer in seconds.\n");
    out.push_str("# TYPE brewster_ntp_peer_latency_seconds gauge\n");
    out.push_str(
        "# HELP brewster_ntp_peer_jitter_seconds Latest latency jitter per peer in seconds.\n",
    );
    out.push_str("# TYPE brewster_ntp_peer_jitter_seconds gauge\n");
    out.push_str("# HELP brewster_ntp_peer_offset_seconds Latest offset per peer in seconds.\n");
    out.push_str("# TYPE brewster_ntp_peer_offset_seconds gauge\n");
    out.push_str(
        "# HELP brewster_ntp_peer_offset_jitter_seconds Latest offset jitter per peer in seconds.\n",
    );
    out.push_str("# TYPE brewster_ntp_peer_offset_jitter_seconds gauge\n");
    out.push_str("# HELP brewster_ntp_peer_last_sync_uptime_seconds Uptime in seconds of the latest successful sync per peer.\n");
    out.push_str("# TYPE brewster_ntp_peer_last_sync_uptime_seconds gauge\n");
    for peer in peer_snapshots {
        let is_selected = peer.source == ntp_source && peer.address == master_ip;
        let selected_str = if is_selected { "true" } else { "false" };
        let source_label = peer.source.label();
        let [a, b, c, d] = peer.address;
        let _ = writeln!(
            out,
            "brewster_ntp_peer_success_total{{source=\"{}\",address=\"{}.{}.{}.{}\",selected=\"{}\"}} {}",
            source_label, a, b, c, d, selected_str, peer.success_count
        );
        let _ = writeln!(
            out,
            "brewster_ntp_peer_fail_total{{source=\"{}\",address=\"{}.{}.{}.{}\",selected=\"{}\"}} {}",
            source_label, a, b, c, d, selected_str, peer.fail_count
        );
        if peer.has_sample {
            let _ = writeln!(
                out,
                "brewster_ntp_peer_jitter_seconds{{source=\"{}\",address=\"{}.{}.{}.{}\",selected=\"{}\"}} {:.6}",
                source_label,
                a,
                b,
                c,
                d,
                selected_str,
                peer.jitter_us as f64 / 1_000_000.0
            );
            let _ = writeln!(
                out,
                "brewster_ntp_peer_last_sync_uptime_seconds{{source=\"{}\",address=\"{}.{}.{}.{}\",selected=\"{}\"}} {}",
                source_label, a, b, c, d, selected_str, peer.last_sync_uptime_s
            );
        }
        if peer.has_sample {
            let _ = writeln!(
                out,
                "brewster_ntp_peer_latency_seconds{{source=\"{}\",address=\"{}.{}.{}.{}\",selected=\"{}\"}} {:.6}",
                source_label,
                a,
                b,
                c,
                d,
                selected_str,
                peer.latency_us as f64 / 1_000_000.0
            );
        }
        if let Some(offset_us) = peer.offset_us {
            let _ = writeln!(
                out,
                "brewster_ntp_peer_offset_seconds{{source=\"{}\",address=\"{}.{}.{}.{}\",selected=\"{}\"}} {:.6}",
                source_label,
                a,
                b,
                c,
                d,
                selected_str,
                offset_us as f64 / 1_000_000.0
            );
        }
        if let Some(offset_jitter_us) = peer.offset_jitter_us {
            let _ = writeln!(
                out,
                "brewster_ntp_peer_offset_jitter_seconds{{source=\"{}\",address=\"{}.{}.{}.{}\",selected=\"{}\"}} {:.6}",
                source_label,
                a,
                b,
                c,
                d,
                selected_str,
                offset_jitter_us as f64 / 1_000_000.0
            );
        }
    }

    // Replace the hardcoded "brewster_" prefix with the device hostname across
    // all HELP, TYPE, and sample lines so metrics are named after the device.
    let prefix = alloc::format!("{}_", device_hostname());
    out.replace("brewster_", &prefix)
}
