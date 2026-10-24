//! Fragmentation plan creation and reassembly, matching
//! `FragmentManager.kt` (100% iOS-compatible thresholds and limits).
//!
//! All size caps are security limits, not tuning knobs; every rejection
//! behavior mirrors the Android implementation.

use std::collections::{BTreeMap, HashMap};

use crate::fragment::{FragmentPayload, FRAGMENT_HEADER_SIZE};
use crate::packet::{self, Packet};
use crate::padding;

/// iOS values: 512 MTU threshold, 469 max fragment payload (512 - headers).
pub const FRAGMENT_SIZE_THRESHOLD: usize = 512;
pub const MAX_FRAGMENT_SIZE: usize = 469;
pub const FRAGMENT_TIMEOUT_MS: u64 = 30_000;
pub const MAX_FRAGMENTS_PER_ID: u16 = 256;
pub const MAX_FRAGMENT_TOTAL_BYTES: usize = 1_048_576;
pub const MAX_ACTIVE_FRAGMENT_SETS: usize = 64;
pub const MAX_GLOBAL_FRAGMENT_TOTAL_BYTES: usize = 4 * 1_048_576;

/// Split `packet` into FRAGMENT packets following the Android/iOS plan:
/// unpad the frame, fragment only when larger than the 512 threshold, size
/// chunks to fit MTU after per-packet overhead, and cap the fragment count.
/// Returns the original packet unmodified when no fragmentation is needed.
pub fn create_fragments(packet: &Packet, max_fragments: usize) -> Vec<Packet> {
    if !(1..=0xFFFF).contains(&max_fragments) {
        return Vec::new();
    }
    // Our encoder never pads; this is equivalent to Kotlin's
    // unpad(packet.toBinaryData()) because encode() adds canonical PKCS#7
    // padding that unpad() strips straight back to the frame bytes.
    let Some(full_data) = packet::encode(packet) else {
        return Vec::new();
    };
    if full_data.len() <= FRAGMENT_SIZE_THRESHOLD {
        return vec![packet.clone()];
    }

    // Dynamic fragment size: 512 minus per-packet overhead, never above 469.
    // Mirrors FragmentManager.createFragments() arithmetic (header 15/13,
    // sender 8, recipient 8, route, 13-byte fragment header, 16-byte padding
    // allowance).
    let has_route = packet.route.is_some();
    let header_size = if has_route { 15 } else { 13 };
    let recipient_size = if packet.recipient_id.is_some() { 8 } else { 0 };
    let route_size = packet.route.as_ref().map_or(0, |r| 1 + r.len() * 8);
    let overhead = header_size + 8 + recipient_size + route_size + 13 + 16;
    let max_data_size = (FRAGMENT_SIZE_THRESHOLD.saturating_sub(overhead)).min(MAX_FRAGMENT_SIZE);
    if max_data_size == 0 {
        return Vec::new();
    }

    let required = full_data.len().div_ceil(max_data_size);
    if required > max_fragments {
        return Vec::new();
    }

    let fragment_id: [u8; 8] = rand_bytes_8();
    let total = required as u16;
    full_data
        .chunks(max_data_size)
        .enumerate()
        .map(|(index, chunk)| {
            let payload = FragmentPayload {
                fragment_id,
                index: index as u16,
                total,
                original_type: packet.packet_type,
                data: chunk.to_vec(),
            };
            Packet {
                version: if has_route { 2 } else { 1 },
                packet_type: crate::MessageType::Fragment as u8,
                sender_id: packet.sender_id,
                recipient_id: packet.recipient_id,
                timestamp: packet.timestamp,
                payload: payload.encode(),
                signature: None, // iOS: signature nil on fragments
                ttl: packet.ttl,
                route: packet.route.clone(),
                wire_payload: None,
            }
        })
        .collect()
}

fn rand_bytes_8() -> [u8; 8] {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Not cryptographic: fragment IDs only correlate chunks within one
    // transmission and collisions are handled by metadata mismatch drops.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut x = nanos as u64 ^ (std::process::id() as u64) << 32;
    let mut out = [0u8; 8];
    for byte in &mut out {
        // xorshift64*
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        *byte = (x.wrapping_mul(0x2545F4914F6CDD1D) >> 56) as u8;
    }
    out
}

struct FragmentSet {
    original_type: u8,
    total: u16,
    created_ms: u64,
    fragments: BTreeMap<u16, Vec<u8>>,
    cumulative: usize,
}

/// Result of offering one FRAGMENT packet payload to the reassembler.
pub enum Reassembly {
    /// More fragments are needed.
    Pending,
    /// The reassembled packet (TTL suppressed to 0, as on Android).
    Complete(Packet),
    /// Rejected; mirrors a `return null` in FragmentManager.handleFragment.
    Rejected,
}

/// Reassembles fragmented packets with the Android security caps.
///
/// Time is injected (`now_ms`) so timeout behavior is deterministic in tests;
/// call `cleanup(now_ms)` periodically like the Android 10-second sweeper.
#[derive(Default)]
pub struct Reassembler {
    sets: HashMap<[u8; 8], FragmentSet>,
    global_buffered_bytes: usize,
}

impl Reassembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn active_sets(&self) -> usize {
        self.sets.len()
    }

    pub fn global_buffered_bytes(&self) -> usize {
        self.global_buffered_bytes
    }

    /// Drop fragment sets older than `FRAGMENT_TIMEOUT_MS`.
    pub fn cleanup(&mut self, now_ms: u64) {
        let expired: Vec<[u8; 8]> = self
            .sets
            .iter()
            .filter(|(_, s)| now_ms.saturating_sub(s.created_ms) > FRAGMENT_TIMEOUT_MS)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            self.remove_set(&id);
        }
    }

    fn remove_set(&mut self, id: &[u8; 8]) {
        if let Some(set) = self.sets.remove(id) {
            self.global_buffered_bytes = self.global_buffered_bytes.saturating_sub(set.cumulative);
        }
    }

    /// Offer a FRAGMENT packet's payload. See `Reassembly` for outcomes.
    pub fn handle_fragment(&mut self, payload: &[u8], now_ms: u64) -> Reassembly {
        if payload.len() < FRAGMENT_HEADER_SIZE {
            return Reassembly::Rejected;
        }
        let Some(fragment) = FragmentPayload::decode(payload) else {
            return Reassembly::Rejected;
        };
        if !fragment.is_valid() {
            return Reassembly::Rejected;
        }
        if fragment.total > MAX_FRAGMENTS_PER_ID {
            return Reassembly::Rejected;
        }

        let id = fragment.fragment_id;
        if let Some(existing) = self.sets.get(&id) {
            if existing.total != fragment.total || existing.original_type != fragment.original_type
            {
                self.remove_set(&id);
                return Reassembly::Rejected;
            }
        }

        let is_new_set = !self.sets.contains_key(&id);
        if is_new_set {
            if self.sets.len() >= MAX_ACTIVE_FRAGMENT_SETS {
                return Reassembly::Rejected;
            }
            self.sets.insert(
                id,
                FragmentSet {
                    original_type: fragment.original_type,
                    total: fragment.total,
                    created_ms: now_ms,
                    fragments: BTreeMap::new(),
                    cumulative: 0,
                },
            );
        }

        let set = match self.sets.get_mut(&id) {
            Some(set) => set,
            None => return Reassembly::Rejected,
        };

        let old_entry_size = set.fragments.get(&fragment.index).map_or(0, Vec::len);
        let new_size = set.cumulative - old_entry_size + fragment.data.len();
        if new_size > MAX_FRAGMENT_TOTAL_BYTES {
            self.remove_set(&id);
            return Reassembly::Rejected;
        }

        let delta = fragment.data.len() as isize - old_entry_size as isize;
        if (self.global_buffered_bytes as isize + delta) as usize > MAX_GLOBAL_FRAGMENT_TOTAL_BYTES
        {
            if is_new_set {
                self.remove_set(&id);
            }
            return Reassembly::Rejected;
        }

        set.fragments.insert(fragment.index, fragment.data);
        set.cumulative = new_size;
        self.global_buffered_bytes = (self.global_buffered_bytes as isize + delta) as usize;

        let expected_total = self.sets.get(&id).map_or(fragment.total, |s| s.total);
        let set = self.sets.get(&id).expect("set presence checked above");
        if set.fragments.len() != expected_total as usize {
            return Reassembly::Pending;
        }

        let mut reassembled = Vec::with_capacity(set.cumulative);
        for index in 0..expected_total {
            if let Some(data) = set.fragments.get(&index) {
                reassembled.extend_from_slice(data);
            }
        }

        match packet::decode(&reassembled) {
            Some(mut decoded) => {
                self.remove_set(&id);
                // Mirrors the Android TTL suppression on reassembled packets.
                decoded.ttl = 0;
                Reassembly::Complete(decoded)
            }
            None => {
                // Android keeps the set on decode failure (it ages out via the
                // 30 s timeout). Mirrored here deliberately: changing this
                // would diverge from peer behavior under hostile fragments.
                Reassembly::Rejected
            }
        }
    }
}

/// Unpad helper re-exported for callers following the create path.
pub fn unpad_frame(data: &[u8]) -> Vec<u8> {
    padding::unpad(data)
}
