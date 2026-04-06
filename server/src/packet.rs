// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) 2026 David Bannister

//! Brewster UDP telemetry packet codec.
//!
//! Wire format — 37 bytes, little-endian (version 2):
//!
//! ```text
//!  [0..4]   magic: b"BREW"
//!  [4]      version: u8             (bump whenever the layout changes)
//!  [5..9]   nonce: u32 LE           (random boot-time nonce; identifies this device session)
//!  [9..13]  seq: u32 LE
//!  [13..17] uptime_s: u32 LE
//!  [17..19] temp_centi[0]: i16 LE  (control probe; i16::MAX = no reading)
//!  [19..21] temp_centi[1]: i16 LE  (sensor 1;      i16::MAX = no reading)
//!  [21..23] temp_centi[2]: i16 LE  (sensor 2;      i16::MAX = no reading)
//!  [23..25] target_centi: i16 LE
//!  [25]     output_pct: u8          (0–100 %)
//!  [26]     flags: u8               bit 0 = relay_on
//!                                   bit 1 = collecting
//!                                   bit 2 = ntp_synced
//!                                   bit 3 = history_clear (one-shot)
//!  [27]     window_step: u8         (0–15)
//!  [28]     on_steps: u8            (0–15)
//!  [29]     sensor_status[0]: u8
//!  [30]     sensor_status[1]: u8
//!  [31]     sensor_status[2]: u8
//!  [32..36] device_ip: [u8; 4]
//!  [36]     sensor_count: u8        (number of configured probes, 1–3)
//! ```

pub const PACKET_MAGIC: &[u8; 4] = b"BREW";
pub const PACKET_VERSION: u8 = 2;
pub const PACKET_SIZE: usize = 37;
pub const TEMP_NONE: i16 = i16::MAX;

#[derive(Debug, Clone)]
pub struct Packet {
    /// Wire format version (always equals [`PACKET_VERSION`] after a successful decode).
    #[allow(dead_code)]
    pub version: u8,
    /// Random nonce generated at ESP32 boot; identifies a device session.
    pub nonce: u32,
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

        let nonce = u32::from_le_bytes(buf[5..9].try_into().ok()?);
        let seq = u32::from_le_bytes(buf[9..13].try_into().ok()?);
        let uptime_s = u32::from_le_bytes(buf[13..17].try_into().ok()?);

        let decode_temp = |bytes: &[u8]| -> f32 {
            let centi = i16::from_le_bytes(bytes.try_into().unwrap());
            if centi == TEMP_NONE {
                f32::NAN
            } else {
                centi as f32 / 100.0
            }
        };

        let temps = [
            decode_temp(&buf[17..19]),
            decode_temp(&buf[19..21]),
            decode_temp(&buf[21..23]),
        ];
        let target_c = i16::from_le_bytes(buf[23..25].try_into().ok()?) as f32 / 100.0;
        let output_pct = buf[25];
        let flags = buf[26];
        let relay_on = flags & 0x01 != 0;
        let collecting = flags & 0x02 != 0;
        let ntp_synced = flags & 0x04 != 0;
        let history_clear = flags & 0x08 != 0;
        let window_step = buf[27];
        let on_steps = buf[28];
        let sensor_status = [buf[29], buf[30], buf[31]];
        let device_ip = [buf[32], buf[33], buf[34], buf[35]];
        let sensor_count = buf[36].clamp(1, 3);

        Some(Packet {
            version,
            nonce,
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

    /// Encode into a 37-byte buffer — used only in tests.
    #[allow(dead_code)]
    pub fn encode(&self) -> [u8; PACKET_SIZE] {
        let mut buf = [0u8; PACKET_SIZE];
        buf[0..4].copy_from_slice(PACKET_MAGIC);
        buf[4] = PACKET_VERSION;
        buf[5..9].copy_from_slice(&self.nonce.to_le_bytes());
        buf[9..13].copy_from_slice(&self.seq.to_le_bytes());
        buf[13..17].copy_from_slice(&self.uptime_s.to_le_bytes());

        let encode_temp = |c: f32| -> i16 {
            if c.is_nan() {
                TEMP_NONE
            } else {
                (c * 100.0).clamp(i16::MIN as f32, (i16::MAX - 1) as f32) as i16
            }
        };
        buf[17..19].copy_from_slice(&encode_temp(self.temps[0]).to_le_bytes());
        buf[19..21].copy_from_slice(&encode_temp(self.temps[1]).to_le_bytes());
        buf[21..23].copy_from_slice(&encode_temp(self.temps[2]).to_le_bytes());
        let target_centi = (self.target_c * 100.0).clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        buf[23..25].copy_from_slice(&target_centi.to_le_bytes());
        buf[25] = self.output_pct;
        buf[26] = (self.relay_on as u8)
            | ((self.collecting as u8) << 1)
            | ((self.ntp_synced as u8) << 2)
            | ((self.history_clear as u8) << 3);
        buf[27] = self.window_step;
        buf[28] = self.on_steps;
        buf[29] = self.sensor_status[0];
        buf[30] = self.sensor_status[1];
        buf[31] = self.sensor_status[2];
        buf[32..36].copy_from_slice(&self.device_ip);
        buf[36] = self.sensor_count;
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
            nonce: 0xdeadbeef,
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
        assert_eq!(decoded.nonce, 0xdeadbeef);
        assert_eq!(decoded.seq, 42);
        assert!((decoded.temps[0] - 21.5).abs() < 0.01);
        assert!(decoded.temps[1].is_nan());
        assert!(decoded.relay_on);
        assert!(decoded.collecting);
        assert!(!decoded.ntp_synced);
        assert_eq!(decoded.sensor_count, 2);
    }
}
