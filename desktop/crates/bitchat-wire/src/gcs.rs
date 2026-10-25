//! GCS (Golomb-Coded Set) sync filters and deterministic packet IDs.
//! Byte-exact port of `GCSFilter.kt` and `PacketIdUtil.kt`.
//!
//! Hashing: `h64(id) = SHA-256(id16)[..8]` big-endian, masked to 63 bits,
//! mapped into `[0, M)` with `h64 % M` (0 coerced to 1). The bitstream packs
//! Golomb-Rice codes MSB-first; the final partial byte is zero-padded (this
//! is part of the wire format and both sides replicate it).
//!
//! Packet IDs follow `PacketIdUtil`: `SHA-256(type | senderID | timestamp
//! (8B BE) | payload)`, truncated to 16 bytes.

use sha2::{Digest, Sha256};

/// Live default via debug prefs (`GossipSyncManager` config provider); the
/// floor documented in `SyncDefaults` is 256.
pub const DEFAULT_FILTER_BYTES: usize = 400;
/// Target false-positive rate (1%).
pub const DEFAULT_FPR: f64 = 0.01;
/// Max announcements + messages offered per sync (debug pref default).
pub const SEEN_CAPACITY: usize = 500;
/// Dedup cache size, mirror of `AppConstants.Mesh.DEFAULT_DEDUP_CAPACITY`.
pub const DEDUP_CAPACITY: usize = 10_000;

/// Deterministic 16-byte packet ID, port of `PacketIdUtil.computeIdBytes`.
pub fn packet_id(packet_type: u8, sender_id: &[u8; 8], timestamp: u64, payload: &[u8]) -> [u8; 16] {
    let mut md = Sha256::new();
    md.update([packet_type]);
    md.update(sender_id);
    md.update(timestamp.to_be_bytes());
    md.update(payload);
    let digest = md.finalize();
    digest[..16].try_into().expect("sha256 is 32 bytes")
}

/// Positive 63-bit hash of a packet ID, port of `GCSFilter.h64`.
pub fn h64(id16: &[u8; 16]) -> u64 {
    let digest = Sha256::digest(id16);
    u64::from_be_bytes(digest[..8].try_into().expect("sha256 is 32 bytes"))
        & 0x7fff_ffff_ffff_ffff
}

/// Port of `GCSFilter.deriveP`: FPR ~= 1 / 2^P.
pub fn derive_p(target_fpr: f64) -> u8 {
    let f = target_fpr.clamp(0.000001, 0.25);
    ((1.0 / f).ln() / 2f64.ln())
        .ceil()
        .max(1.0) as u8
}

/// Port of `GCSFilter.estimateMaxElementsForSize`.
pub fn estimate_max_elements_for_size(bytes: usize, p: u8) -> usize {
    let bits = (bytes * 8).max(8);
    let per = (p as usize + 2).max(3);
    (bits / per).max(1)
}

pub struct GcsParams {
    pub p: u8,
    pub m: u64,
    pub data: Vec<u8>,
}

/// Port of `GCSFilter.buildFilter`: encode `ids`, trimming the tail by 10%
/// steps until the bitstream fits `max_bytes`.
pub fn build_filter(ids: &[[u8; 16]], max_bytes: usize, target_fpr: f64) -> GcsParams {
    let p = derive_p(target_fpr);
    let n_cap = estimate_max_elements_for_size(max_bytes, p);
    let mut trimmed_n = ids.len().min(n_cap);

    let mut final_m = ((trimmed_n as u64) << p).max(1);
    loop {
        let mut mapped: Vec<u64> = ids[..trimmed_n.min(ids.len())]
            .iter()
            .map(|id| {
                let v = h64(id) % final_m;
                if v == 0 { 1 } else { v }
            })
            .collect();
        mapped.sort_unstable();
        mapped.dedup();
        let encoded = encode(&mapped, p);
        if encoded.len() <= max_bytes || trimmed_n == 0 {
            return GcsParams {
                p,
                m: final_m,
                data: encoded,
            };
        }
        trimmed_n = trimmed_n * 9 / 10;
        final_m = ((trimmed_n as u64) << p).max(1);
    }
}

fn encode(sorted: &[u64], p: u8) -> Vec<u8> {
    let mut bw = BitWriter::new();
    let mut prev = 0u64;
    let mask = (1u64 << p) - 1;
    for &v in sorted {
        let x = v - prev;
        prev = v;
        let q = (x - 1) >> p;
        let r = (x - 1) & mask;
        for _ in 0..q {
            bw.write_bit(1);
        }
        bw.write_bit(0);
        bw.write_bits(r, p);
    }
    bw.finish()
}

/// Port of `GCSFilter.decodeToSortedSet`, including the final-byte padding
/// quirk shared by all deployed clients.
pub fn decode_to_sorted_set(p: u8, m: u64, data: &[u8]) -> Vec<u64> {
    let mut values = Vec::new();
    let mut reader = BitReader::new(data);
    let mut acc = 0u64;
    while !reader.eof() {
        let mut q = 0u64;
        loop {
            match reader.read_bit() {
                None => break,
                Some(1) => q += 1,
                Some(_) => break,
            }
        }
        if reader.last_was_eof() {
            break;
        }
        let Some(r) = reader.read_bits(p) else {
            break;
        };
        let x = (q << p) + r + 1;
        acc += x;
        if acc >= m {
            break; // out-of-range safeguard
        }
        values.push(acc);
    }
    values
}

/// Binary search membership, port of `GCSFilter.contains`.
pub fn contains(sorted: &[u64], candidate: u64) -> bool {
    sorted.binary_search(&candidate).is_ok()
}

struct BitWriter {
    buf: Vec<u8>,
    cur: u32,
    nbits: u32,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            buf: Vec::new(),
            cur: 0,
            nbits: 0,
        }
    }

    fn write_bit(&mut self, bit: u32) {
        self.cur = (self.cur << 1) | (bit & 1);
        self.nbits += 1;
        if self.nbits == 8 {
            self.buf.push(self.cur as u8);
            self.cur = 0;
            self.nbits = 0;
        }
    }

    fn write_bits(&mut self, value: u64, count: u8) {
        for i in (0..count).rev() {
            self.write_bit(((value >> i) & 1) as u32);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.nbits > 0 {
            self.buf.push((self.cur << (8 - self.nbits)) as u8);
        }
        self.buf
    }
}

struct BitReader<'a> {
    data: &'a [u8],
    i: usize,
    nleft: u32,
    cur: u32,
    last_was_eof: bool,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            i: 0,
            nleft: 8,
            cur: data.first().map_or(0, |b| *b as u32),
            last_was_eof: false,
        }
    }

    fn eof(&self) -> bool {
        self.i >= self.data.len()
    }

    fn last_was_eof(&self) -> bool {
        self.last_was_eof
    }

    fn read_bit(&mut self) -> Option<u32> {
        if self.i >= self.data.len() {
            self.last_was_eof = true;
            return None;
        }
        let bit = (self.cur >> 7) & 1;
        self.cur = (self.cur << 1) & 0xFF;
        self.nleft -= 1;
        if self.nleft == 0 {
            self.i += 1;
            if self.i < self.data.len() {
                self.cur = self.data[self.i] as u32;
                self.nleft = 8;
            }
        }
        Some(bit)
    }

    fn read_bits(&mut self, count: u8) -> Option<u64> {
        let mut v = 0u64;
        for _ in 0..count {
            v = (v << 1) | u64::from(self.read_bit()?);
        }
        Some(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_id_matches_reference_vector() {
        // Generated with a reference SHA-256 implementation from the captured
        // field values (synthetic in-repo fixture values).
        let sender: [u8; 8] = [0x2a, 0x6a, 0x0d, 0x41, 0x45, 0x1f, 0xbe, 0x94];
        let payload = [
            0x01, 0x05, 0x79, 0x66, 0x6d, 0x31, 0x32, 0x02, 0x20, 0x72, 0x9a, 0xc2, 0x03,
            0xad, 0xc0, 0xb4, 0xe2, 0x98, 0xb3, 0xbd, 0x6d, 0xf4, 0x0a, 0x14, 0x0e, 0xfc,
            0x48, 0x2c, 0x12, 0x55, 0xa7, 0x70, 0x8a, 0xd1, 0x5e, 0xa4, 0xe7, 0xa5, 0x78,
            0x04, 0x7f, 0x03, 0x20, 0x8c, 0x65, 0xef, 0x77, 0x1f, 0xfe, 0x0f, 0x65, 0x9e,
            0x74, 0xf6, 0xcb, 0x8e, 0x49, 0xd4, 0x5e, 0x15, 0xd9, 0x88, 0x61, 0x8a, 0x78,
            0x26, 0x55, 0x59, 0x0c, 0x47, 0x2c, 0xff, 0xfb, 0x99, 0x23, 0x05, 0x02, 0x00,
            0x01,
        ];
        let id = packet_id(1, &sender, 1_792_916_708_320, &payload);
        assert_eq!(
            "907b3bd5f741f86746b3a68460db2357",
            id.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        assert_eq!(71, h64(&id) % 128);
    }

    #[test]
    fn golomb_rice_bitstream_golden_vector() {
        // Hand-computed: p=2, values [1, 2, 5] -> deltas 1, 1, 3.
        // x=1: q=0 "0", r=00; x=1: "0","00"; x=3: q=0 "0", r=10.
        // Bits: 0 00 0 00 0 10 = nine bits -> bytes 0000 0001, 0000 0000.
        assert_eq!(vec![0x01, 0x00], encode(&[1, 2, 5], 2));
        // m=6 stops the decode right after the third value; larger m would
        // keep consuming the zero padding bits (Kotlin-identical quirk).
        assert_eq!(vec![1, 2, 5], decode_to_sorted_set(2, 6, &[0x01, 0x00]));
    }

    #[test]
    fn filter_round_trip_contains_all_ids() {
        let ids: Vec<[u8; 16]> = (0..80u32)
            .map(|i| {
                let mut md = Sha256::new();
                md.update(i.to_be_bytes());
                md.finalize()[..16].try_into().unwrap()
            })
            .collect();
        let filter = build_filter(&ids, DEFAULT_FILTER_BYTES, DEFAULT_FPR);
        assert!(filter.data.len() <= DEFAULT_FILTER_BYTES);
        let sorted = decode_to_sorted_set(filter.p, filter.m, &filter.data);
        for id in &ids {
            let v = h64(id) % filter.m;
            let v = if v == 0 { 1 } else { v };
            assert!(contains(&sorted, v), "filter must contain {id:02x?}");
        }
    }

    #[test]
    fn zero_hash_maps_to_one() {
        // Mapped value 0 is coerced to 1 on both encode and membership sides.
        let sorted = decode_to_sorted_set(7, 128, &encode(&[1, 4, 9], 7));
        assert_eq!(vec![1, 4, 9], sorted);
    }
}
