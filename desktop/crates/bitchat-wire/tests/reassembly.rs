//! Reassembly behavior tests mirroring `FragmentManager.kt` semantics:
//! security caps, metadata consistency, TTL suppression, and timeouts.

use bitchat_wire::fragment::FragmentPayload;
use bitchat_wire::packet::{self, MessageType, Packet};
use bitchat_wire::reassembly::{
    create_fragments, Reassembler, Reassembly, FRAGMENT_SIZE_THRESHOLD, MAX_ACTIVE_FRAGMENT_SETS,
    MAX_FRAGMENTS_PER_ID, MAX_FRAGMENT_TOTAL_BYTES, MAX_GLOBAL_FRAGMENT_TOTAL_BYTES,
};

fn big_packet(payload_size: usize) -> Packet {
    // Incompressible pseudo-random payload (SplitMix64-style, no rand
    // dependency) so encode() does not deflate it below the 512 threshold
    // before fragmentation planning, matching how the Kotlin tests behave
    // with real high-entropy payloads.
    let mut state = 0x9E3779B97F4A7C15u64;
    let payload: Vec<u8> = (0..payload_size)
        .map(|_| {
            state = state.wrapping_add(0x9E3779B97F4A7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
            z ^= z >> 31;
            (z >> 56) as u8
        })
        .collect();
    let mut p = Packet::new(MessageType::Message as u8, 7, [0xAA; 8], payload);
    p.timestamp = 12345;
    p
}

#[test]
fn create_fragments_splits_only_above_threshold() {
    // 490-byte payload -> 14-byte v1 header + 8-byte sender + 490 = 512
    // encoded bytes: exactly at the threshold, no fragmentation.
    let small = big_packet(FRAGMENT_SIZE_THRESHOLD - 14 - 8);
    let fragments = create_fragments(&small, 0xFFFF);
    assert_eq!(1, fragments.len());
    assert_eq!(small, fragments[0]);

    let large = big_packet(FRAGMENT_SIZE_THRESHOLD * 4);
    let fragments = create_fragments(&large, 0xFFFF);
    assert!(fragments.len() > 1);
    assert!(fragments.len() <= 4 * 4); // bounded by chunk sizing
    assert!(fragments
        .iter()
        .all(|f| f.packet_type == MessageType::Fragment as u8));
    assert!(fragments.iter().all(|f| f.signature.is_none()));
    assert!(fragments.iter().all(|f| f.payload.len() <= 13 + 469));
}

#[test]
fn round_trip_reassembles_in_any_order_with_ttl_suppressed() {
    let original = big_packet(600);
    let fragments = create_fragments(&original, 0xFFFF);
    assert!(fragments.len() > 1);

    let mut reassembler = Reassembler::new();
    let mut ordered: Vec<&Packet> = fragments.iter().collect();
    ordered.reverse(); // deliver out of order

    let mut outcome = Reassembly::Pending;
    for (i, f) in ordered.iter().enumerate() {
        outcome = reassembler.handle_fragment(&f.payload, i as u64 * 100);
    }
    let Reassembly::Complete(decoded) = outcome else {
        panic!("expected complete reassembly");
    };
    assert_eq!(0, decoded.ttl, "reassembled TTL must be suppressed");
    assert_eq!(original.packet_type, decoded.packet_type);
    assert_eq!(original.sender_id, decoded.sender_id);
    assert_eq!(original.timestamp, decoded.timestamp);
    assert_eq!(original.payload, decoded.payload);
}

#[test]
fn rejects_invalid_and_oversized_fragments() {
    let mut reassembler = Reassembler::new();
    assert!(matches!(
        reassembler.handle_fragment(&[0u8; 12], 0),
        Reassembly::Rejected
    ));

    // total above MAX_FRAGMENTS_PER_ID
    let too_many = FragmentPayload {
        fragment_id: [1; 8],
        index: 0,
        total: MAX_FRAGMENTS_PER_ID + 1,
        original_type: 0x02,
        data: vec![0xAA; 469],
    };
    assert!(matches!(
        reassembler.handle_fragment(&too_many.encode(), 0),
        Reassembly::Rejected
    ));
}

#[test]
fn metadata_mismatch_drops_the_whole_set() {
    let mut reassembler = Reassembler::new();
    let f1 = FragmentPayload {
        fragment_id: [7; 8],
        index: 0,
        total: 2,
        original_type: 0x02,
        data: vec![1; 100],
    };
    assert!(matches!(
        reassembler.handle_fragment(&f1.encode(), 0),
        Reassembly::Pending
    ));

    // Same ID, different total: set is dropped and the fragment rejected.
    let conflicting = FragmentPayload {
        fragment_id: [7; 8],
        index: 1,
        total: 3,
        original_type: 0x02,
        data: vec![2; 100],
    };
    assert!(matches!(
        reassembler.handle_fragment(&conflicting.encode(), 100),
        Reassembly::Rejected
    ));
    assert_eq!(0, reassembler.active_sets());
}

#[test]
fn per_set_total_bytes_cap_drops_the_set() {
    let mut reassembler = Reassembler::new();
    let first = FragmentPayload {
        fragment_id: [9; 8],
        index: 0,
        total: 2,
        original_type: 0x02,
        data: vec![0u8; MAX_FRAGMENT_TOTAL_BYTES],
    };
    assert!(matches!(
        reassembler.handle_fragment(&first.encode(), 0),
        Reassembly::Pending
    ));
    // Second fragment pushes the cumulative total over the 1 MiB cap.
    let second = FragmentPayload {
        fragment_id: [9; 8],
        index: 1,
        total: 2,
        original_type: 0x02,
        data: vec![1u8; 1],
    };
    assert!(matches!(
        reassembler.handle_fragment(&second.encode(), 100),
        Reassembly::Rejected
    ));
    assert_eq!(0, reassembler.active_sets());
}

#[test]
fn active_set_cap_rejects_new_sets() {
    let mut reassembler = Reassembler::new();
    for n in 0..MAX_ACTIVE_FRAGMENT_SETS {
        let f = FragmentPayload {
            fragment_id: [n as u8; 8],
            index: 0,
            total: 2,
            original_type: 0x02,
            data: vec![1; 10],
        };
        assert!(matches!(
            reassembler.handle_fragment(&f.encode(), 0),
            Reassembly::Pending
        ));
    }
    assert_eq!(MAX_ACTIVE_FRAGMENT_SETS, reassembler.active_sets());

    let extra = FragmentPayload {
        fragment_id: [0xEE; 8],
        index: 0,
        total: 2,
        original_type: 0x02,
        data: vec![1; 10],
    };
    assert!(matches!(
        reassembler.handle_fragment(&extra.encode(), 0),
        Reassembly::Rejected
    ));
}

#[test]
fn global_buffered_bytes_cap_rejects_new_data() {
    let mut reassembler = Reassembler::new();
    // Four sets at the per-set cap (1 MiB each) reach the 4 MiB global cap;
    // a fifth set is admitted, then its first fragment is rejected and the
    // empty set is rolled back.
    for n in 0..4u8 {
        let f = FragmentPayload {
            fragment_id: [n; 8],
            index: 0,
            total: 2,
            original_type: 0x02,
            data: vec![0u8; MAX_FRAGMENT_TOTAL_BYTES],
        };
        assert!(matches!(
            reassembler.handle_fragment(&f.encode(), 0),
            Reassembly::Pending
        ));
    }
    assert_eq!(
        MAX_GLOBAL_FRAGMENT_TOTAL_BYTES,
        reassembler.global_buffered_bytes()
    );
    assert!(reassembler.active_sets() < MAX_ACTIVE_FRAGMENT_SETS);

    let extra = FragmentPayload {
        fragment_id: [0xEE; 8],
        index: 0,
        total: 2,
        original_type: 0x02,
        data: vec![1; 10],
    };
    let outcome = reassembler.handle_fragment(&extra.encode(), 1000);
    assert!(matches!(outcome, Reassembly::Rejected));
    assert_eq!(
        4,
        reassembler.active_sets(),
        "empty new set must be rolled back"
    );
}

#[test]
fn timeout_cleanup_releases_stale_sets_and_bytes() {
    let mut reassembler = Reassembler::new();
    let f = FragmentPayload {
        fragment_id: [3; 8],
        index: 0,
        total: 2,
        original_type: 0x02,
        data: vec![5; 500],
    };
    reassembler.handle_fragment(&f.encode(), 1_000);
    assert_eq!(1, reassembler.active_sets());
    assert_eq!(500, reassembler.global_buffered_bytes());

    // Not yet stale at the timeout boundary.
    reassembler.cleanup(1_000 + 30_000);
    assert_eq!(1, reassembler.active_sets());
    // Stale just past it.
    reassembler.cleanup(1_000 + 30_001);
    assert_eq!(0, reassembler.active_sets());
    assert_eq!(0, reassembler.global_buffered_bytes());
}

#[test]
fn reassembled_packet_uses_full_packet_decoder() {
    // Fragment payloads that decode to garbage must not complete, and the set
    // stays resident until timeout (mirrors Android).
    let mut reassembler = Reassembler::new();
    let garbage: Vec<u8> = (0..600u16).map(|i| i as u8).collect();
    let fragments = FragmentPayload::split([8; 8], 0x02, &garbage, 469);
    assert!(fragments.len() > 1);

    let mut last = Reassembly::Pending;
    for f in &fragments {
        last = reassembler.handle_fragment(&f.encode(), 0);
    }
    assert!(matches!(last, Reassembly::Rejected));
    assert_eq!(1, reassembler.active_sets(), "set remains until timeout");
}

#[test]
fn fragment_count_above_caller_cap_is_rejected() {
    let original = big_packet(FRAGMENT_SIZE_THRESHOLD * 4);
    let fragments = create_fragments(&original, 2);
    assert!(fragments.is_empty());
}

#[test]
fn legacy_decode_still_works_for_unfragmented_packets() {
    let original = big_packet(100);
    let wire = packet::encode(&original).unwrap();
    assert_eq!(original, packet::decode(&wire).unwrap());
}
