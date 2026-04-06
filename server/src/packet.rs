// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

//! Brewster UDP telemetry packet codec.
//!
//! Wire format — 57 bytes, little-endian (version 5):
//!
//! ```text
//!  [0..4]   magic: b"BREW"
//!  [4]      version: u8             (bump whenever the layout changes)
//!  [5..25]  hostname: [u8; 20]      (device hostname, null-padded UTF-8, max 20 chars)
//!  [25..29] seq: u32 LE
//!  [29..33] uptime_s: u32 LE
//!  [33..35] temp_centi[0]: i16 LE  (control probe; i16::MAX = no reading)
//!  [35..37] temp_centi[1]: i16 LE  (sensor 1;      i16::MAX = no reading)
//!  [37..39] temp_centi[2]: i16 LE  (sensor 2;      i16::MAX = no reading)
//!  [39..41] target_centi: i16 LE
//!  [41]     output_pct: u8          (0–100 %)
//!  [42]     flags: u8               bit 0 = relay_on (cool SSR)
//!                                   bit 1 = collecting
//!                                   bit 2 = ntp_synced
//!                                   bit 3 = history_clear (one-shot)
//!                                   bit 4 = heat_on (heat SSR)
//!  [43]     window_step: u8         (0–15)
//!  [44]     on_steps: u8            (0–15)
//!  [45]     sensor_status[0]: u8
//!  [46]     sensor_status[1]: u8
//!  [47]     sensor_status[2]: u8
//!  [48..52] device_ip: [u8; 4]
//!  [52]     sensor_count: u8        (number of configured probes, 1–3)
//!  [53]     deadband_centi: u8      (total dead zone width in 0.01 °C steps, 0–2.55 °C)
//!  [54]     pid_p_pct: i8           (active PID proportional term, %)
//!  [55]     pid_i_pct: i8           (active PID integral term, %)
//!  [56]     pid_d_pct: i8           (active PID derivative term, %)
//! ```

pub const PACKET_MAGIC: &[u8; 4] = b"BREW";
pub const PACKET_VERSION: u8 = 5;
pub const PACKET_SIZE: usize = 57;
#[allow(dead_code)]
pub const HOSTNAME_LEN: usize = 20;
pub const TEMP_NONE: i16 = i16::MAX;

#[derive(Debug, Clone)]
pub struct Packet {
    /// Wire format version (always equals [`PACKET_VERSION`] after a successful decode).
    #[allow(dead_code)]
    pub version: u8,
    /// Device hostname, null-padded to 20 bytes; uniquely identifies the sending device.
    pub hostname: [u8; 20],
    pub seq: u32,
    pub uptime_s: u32,
    /// Centidegrees for each probe; `f32::NAN` when unavailable.
    pub temps: [f32; 3],
    pub target_c: f32,
    pub output_pct: u8,
    pub relay_on: bool,
    pub collecting: bool,
    pub ntp_synced: bool,
    /// Set in the first packet after the device clears its history.
    /// The server uses this to clear its own ring buffer.
    pub history_clear: bool,
    pub heat_on: bool,
    pub window_step: u8,
    pub on_steps: u8,
    /// Raw status codes per sensor (0 = ok).
    pub sensor_status: [u8; 3],
    pub device_ip: [u8; 4],
    pub sensor_count: u8,
    /// Total dead zone width in °C (neither relay fires within ±deadband_c/2 of target).
    pub deadband_c: f32,
    /// Active PID proportional term contribution (%).
    pub pid_p_pct: i8,
    /// Active PID integral term contribution (%).
    pub pid_i_pct: i8,
    /// Active PID derivative term contribution (%).
    pub pid_d_pct: i8,
}

impl Packet {
    /// Decode a 32-byte UDP payload.  Returns `None` if magic is wrong or
    /// the buffer is too short.
    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < PACKET_SIZE {
            return None;
        }
        if &buf[0..4] != PACKET_MAGIC {
            return None;
        }
        let version = buf[4];
        if version != PACKET_VERSION {
            return None;
        }

        let mut hostname = [0u8; 20];
        hostname.copy_from_slice(&buf[5..25]);
        let seq = u32::from_le_bytes(buf[25..29].try_into().ok()?);
        let uptime_s = u32::from_le_bytes(buf[29..33].try_into().ok()?);

        let decode_temp = |bytes: &[u8]| -> f32 {
            let centi = i16::from_le_bytes(bytes.try_into().unwrap());
            if centi == TEMP_NONE {
                f32::NAN
            } else {
                centi as f32 / 100.0
            }
        };

        let temps = [
            decode_temp(&buf[33..35]),
            decode_temp(&buf[35..37]),
            decode_temp(&buf[37..39]),
        ];
        let target_c = i16::from_le_bytes(buf[39..41].try_into().ok()?) as f32 / 100.0;
        let output_pct = buf[41];
        let flags = buf[42];
        let relay_on = flags & 0x01 != 0;
        let collecting = flags & 0x02 != 0;
        let ntp_synced = flags & 0x04 != 0;
        let history_clear = flags & 0x08 != 0;
        let heat_on = flags & 0x10 != 0;
        let window_step = buf[43];
        let on_steps = buf[44];
        let sensor_status = [buf[45], buf[46], buf[47]];
        let device_ip = [buf[48], buf[49], buf[50], buf[51]];
        let sensor_count = buf[52].clamp(1, 3);
        let deadband_c = buf[53] as f32 / 100.0;
        let pid_p_pct = buf[54] as i8;
        let pid_i_pct = buf[55] as i8;
        let pid_d_pct = buf[56] as i8;

        Some(Packet {
            version,
            hostname,
            seq,
            uptime_s,
            temps,
            target_c,
            output_pct,
            relay_on,
            collecting,
            ntp_synced,
            history_clear,
            heat_on,
            window_step,
            on_steps,
            sensor_status,
            device_ip,
            sensor_count,
            deadband_c,
            pid_p_pct,
            pid_i_pct,
            pid_d_pct,
        })
    }

    /// Encode into a 54-byte buffer — used only in tests.
    #[allow(dead_code)]
    pub fn encode(&self) -> [u8; PACKET_SIZE] {
        let mut buf = [0u8; PACKET_SIZE];
        buf[0..4].copy_from_slice(PACKET_MAGIC);
        buf[4] = PACKET_VERSION;
        buf[5..25].copy_from_slice(&self.hostname);
        buf[25..29].copy_from_slice(&self.seq.to_le_bytes());
        buf[29..33].copy_from_slice(&self.uptime_s.to_le_bytes());

        let encode_temp = |c: f32| -> i16 {
            if c.is_nan() {
                TEMP_NONE
            } else {
                (c * 100.0).clamp(i16::MIN as f32, (i16::MAX - 1) as f32) as i16
            }
        };
        buf[33..35].copy_from_slice(&encode_temp(self.temps[0]).to_le_bytes());
        buf[35..37].copy_from_slice(&encode_temp(self.temps[1]).to_le_bytes());
        buf[37..39].copy_from_slice(&encode_temp(self.temps[2]).to_le_bytes());
        let target_centi = (self.target_c * 100.0).clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        buf[39..41].copy_from_slice(&target_centi.to_le_bytes());
        buf[41] = self.output_pct;
        buf[42] = (self.relay_on as u8)
            | ((self.collecting as u8) << 1)
            | ((self.ntp_synced as u8) << 2)
            | ((self.history_clear as u8) << 3);
        buf[43] = self.window_step;
        buf[44] = self.on_steps;
        buf[45] = self.sensor_status[0];
        buf[46] = self.sensor_status[1];
        buf[47] = self.sensor_status[2];
        buf[48..52].copy_from_slice(&self.device_ip);
        buf[52] = self.sensor_count;
        buf[53] = (self.deadband_c * 100.0).clamp(0.0, 255.0) as u8;
        buf[54] = self.pid_p_pct as u8;
        buf[55] = self.pid_i_pct as u8;
        buf[56] = self.pid_d_pct as u8;
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let pkt = Packet {
            version: PACKET_VERSION,
            hostname: *b"brewster\0\0\0\0\0\0\0\0\0\0\0\0",
            seq: 42,
            uptime_s: 3600,
            temps: [21.5, f32::NAN, 19.25],
            target_c: -2.0,
            output_pct: 55,
            relay_on: true,
            collecting: true,
            ntp_synced: false,
            history_clear: false,
            heat_on: false,
            window_step: 3,
            on_steps: 5,
            sensor_status: [0, 255, 0],
            device_ip: [192, 168, 1, 100],
            sensor_count: 2,
            deadband_c: 0.5,
            pid_p_pct: 14,
            pid_i_pct: 3,
            pid_d_pct: -2,
        };
        let buf = pkt.encode();
        assert_eq!(buf.len(), PACKET_SIZE);
        let decoded = Packet::decode(&buf).unwrap();
        assert_eq!(&decoded.hostname[..8], b"brewster");
        assert_eq!(decoded.hostname[8..], [0u8; 12]);
        assert_eq!(decoded.seq, 42);
        assert!((decoded.temps[0] - 21.5).abs() < 0.01);
        assert!(decoded.temps[1].is_nan());
        assert!(decoded.relay_on);
        assert!(decoded.collecting);
        assert!(!decoded.ntp_synced);
        assert_eq!(decoded.sensor_count, 2);
    }
}
