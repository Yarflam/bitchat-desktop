//! REQUEST_SYNC payload using GCS (Golomb-Coded Set) parameters.
//! Byte-exact port of `RequestSyncPacket.kt`.
//!
//! TLV layout with 2-byte big-endian lengths:
//! - `0x01`: P (uint8) — Golomb-Rice parameter
//! - `0x02`: M (uint32, big-endian) — hash range (N * 2^P)
//! - `0x03`: data (opaque) — GR bitstream bytes

/// Receiver-side safety limit, mirror of `SyncDefaults.MAX_ACCEPT_FILTER_BYTES`.
pub const MAX_ACCEPT_FILTER_BYTES: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestSyncPacket {
    pub p: u8,
    pub m: u32,
    pub data: Vec<u8>,
}

impl RequestSyncPacket {
    pub fn new(p: u8, m: u32, data: Vec<u8>) -> Self {
        Self { p, m, data }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_tlv(&mut out, 0x01, &[self.p]);
        put_tlv(&mut out, 0x02, &self.m.to_be_bytes());
        put_tlv(&mut out, 0x03, &self.data);
        out
    }

    /// Unknown TLV types are skipped for forward compatibility, matching the
    /// Kotlin `when (t)` fall-through. Trailing bytes shorter than a TLV
    /// header (3 bytes) are ignored by the loop bound, as on Android.
    pub fn decode(data: &[u8]) -> Option<Self> {
        let mut offset = 0usize;
        let mut p = None;
        let mut m = None;
        let mut payload = None;

        while offset + 3 <= data.len() {
            let tlv_type = data[offset];
            offset += 1;
            let length = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
            offset += 2;
            if offset + length > data.len() {
                return None;
            }
            let value = &data[offset..offset + length];
            offset += length;
            match tlv_type {
                0x01 if length == 1 => p = Some(value[0]),
                0x02 if length == 4 => {
                    m = Some(u32::from_be_bytes([value[0], value[1], value[2], value[3]]));
                }
                0x03 => {
                    if length > MAX_ACCEPT_FILTER_BYTES {
                        return None;
                    }
                    payload = Some(value.to_vec());
                }
                _ => {}
            }
        }

        let p = p?;
        let m = m?;
        let data = payload?;
        if p < 1 || m == 0 {
            return None;
        }
        Some(Self { p, m, data })
    }
}

fn put_tlv(out: &mut Vec<u8>, tlv_type: u8, value: &[u8]) {
    out.push(tlv_type);
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value);
}
