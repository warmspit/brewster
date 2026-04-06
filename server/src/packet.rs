// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

//! Brewster UDP telemetry packet codec.
//!
//! Wire format — 33 bytes, little-endian (version 1):
//!
//! ```text
//!  [0..4]   magic: b"BREW"
//!  [4]      version: u8             (bump whenever the layout changes)
//!  [5..9]   seq: u32 LE
//!  [9..13]  uptime_s: u32 LE
//!  [13..15] temp_centi[0]: i16 LE  (control probe; i16::MAX = no reading)
//!  [15..17] temp_centi[1]: i16 LE  (sensor 1;      i16::MAX = no reading)
//!  [17..19] temp_centi[2]: i16 LE  (sensor 2;      i16::MAX = no reading)
//!  [19..21] target_centi: i16 LE
//!  [21]     output_pct: u8          (0–100 %)
//!  [22]     flags: u8               bit 0 = relay_on
//!                                   bit 1 = collecting
//!                                   bit 2 = ntp_synced
//!                                   bit 3 = history_clear (one-shot)
//!  [23]     window_step: u8         (0–15)
//!  [24]     on_steps: u8            (0–15)
//!  [25]     sensor_status[0]: u8
//!  [26]     sensor_status[1]: u8
//!  [27]     sensor_status[2]: u8
//!  [28..32] device_ip: [u8; 4]
//!  [32]     sensor_count: u8        (number of configured probes, 1–3)
//! ```

pub const PACKET_MAGIC: &[u8; 4] = b"BREW";
pub const PACKET_VERSION: u8 = 1;
pub const PACKET_SIZE: usize = 33;
pub const TEMP_NONE: i16 = i16::MAX;

#[derive(Debug, Clone)]
pub struct Packet {
    /// Wire format version (always equals [`PACKET_VERSION`] after a successful decode).
    #[allow(dead_code)]
    pub version: u8,
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
    pub window_step: u8,
    pub on_steps: u8,
    /// Raw status codes per sensor (0 = ok).
    pub sensor_status: [u8; 3],
    pub device_ip: [u8; 4],
    pub sensor_count: u8,
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

        let seq = u32::from_le_bytes(buf[5..9].try_into().ok()?);
        let uptime_s = u32::from_le_bytes(buf[9..13].try_into().ok()?);

        let decode_temp = |bytes: &[u8]| -> f32 {
            let centi = i16::from_le_bytes(bytes.try_into().unwrap());
            if centi == TEMP_NONE {
                f32::NAN
            } else {
                centi as f32 / 100.0
            }
        };

        let temps = [
            decode_temp(&buf[13..15]),
            decode_temp(&buf[15..17]),
            decode_temp(&buf[17..19]),
        ];
        let target_c = i16::from_le_bytes(buf[19..21].try_into().ok()?) as f32 / 100.0;
        let output_pct = buf[21];
        let flags = buf[22];
        let relay_on = flags & 0x01 != 0;
        let collecting = flags & 0x02 != 0;
        let ntp_synced = flags & 0x04 != 0;
        let history_clear = flags & 0x08 != 0;
        let window_step = buf[23];
        let on_steps = buf[24];
        let sensor_status = [buf[25], buf[26], buf[27]];
        let device_ip = [buf[28], buf[29], buf[30], buf[31]];
        let sensor_count = buf[32].clamp(1, 3);

        Some(Packet {
            version,
            seq,
            uptime_s,
            temps,
            target_c,
            output_pct,
            relay_on,
            collecting,
            ntp_synced,
            history_clear,
            window_step,
            on_steps,
            sensor_status,
            device_ip,
            sensor_count,
        })
    }

    /// Encode into a 33-byte buffer — used only in tests.
    #[allow(dead_code)]
    pub fn encode(&self) -> [u8; PACKET_SIZE] {
        let mut buf = [0u8; PACKET_SIZE];
        buf[0..4].copy_from_slice(PACKET_MAGIC);
        buf[4] = PACKET_VERSION;
        buf[5..9].copy_from_slice(&self.seq.to_le_bytes());
        buf[9..13].copy_from_slice(&self.uptime_s.to_le_bytes());

        let encode_temp = |c: f32| -> i16 {
            if c.is_nan() {
                TEMP_NONE
            } else {
                (c * 100.0).clamp(i16::MIN as f32, (i16::MAX - 1) as f32) as i16
            }
        };
        buf[13..15].copy_from_slice(&encode_temp(self.temps[0]).to_le_bytes());
        buf[15..17].copy_from_slice(&encode_temp(self.temps[1]).to_le_bytes());
        buf[17..19].copy_from_slice(&encode_temp(self.temps[2]).to_le_bytes());
        let target_centi = (self.target_c * 100.0).clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        buf[19..21].copy_from_slice(&target_centi.to_le_bytes());
        buf[21] = self.output_pct;
        buf[22] = (self.relay_on as u8)
            | ((self.collecting as u8) << 1)
            | ((self.ntp_synced as u8) << 2)
            | ((self.history_clear as u8) << 3);
        buf[23] = self.window_step;
        buf[24] = self.on_steps;
        buf[25] = self.sensor_status[0];
        buf[26] = self.sensor_status[1];
        buf[27] = self.sensor_status[2];
        buf[28..32].copy_from_slice(&self.device_ip);
        buf[32] = self.sensor_count;
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
            seq: 42,
            uptime_s: 3600,
            temps: [21.5, f32::NAN, 19.25],
            target_c: -2.0,
            output_pct: 55,
            relay_on: true,
            collecting: true,
            ntp_synced: false,
            window_step: 3,
            on_steps: 5,
            sensor_status: [0, 255, 0],
            device_ip: [192, 168, 1, 100],
            sensor_count: 2,
        };
        let buf = pkt.encode();
        assert_eq!(buf.len(), PACKET_SIZE);
        let decoded = Packet::decode(&buf).unwrap();
        assert_eq!(decoded.seq, 42);
        assert!((decoded.temps[0] - 21.5).abs() < 0.01);
        assert!(decoded.temps[1].is_nan());
        assert!(decoded.relay_on);
        assert!(decoded.collecting);
        assert!(!decoded.ntp_synced);
        assert_eq!(decoded.sensor_count, 2);
    }
}
