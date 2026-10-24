//! Golden wire vectors for the packet framing.
//!
//! Literal-byte assertions ported from
//! `ClientRewriteWireContractTest.kt`. These pin byte-exact compatibility
//! with the Android client; encode/decode round trips alone can hide matching
//! bugs in both directions.

use bitchat_wire::packet;
use bitchat_wire::MessageType;

fn hex(value: &str) -> Vec<u8> {
    assert!(value.len().is_multiple_of(2));
    value
        .as_bytes()
        .chunks(2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect()
}

fn packet(
    version: u8,
    packet_type: u8,
    ttl: u8,
    timestamp: u64,
    sender: &str,
    payload: &str,
) -> packet::Packet {
    let mut sender_id = [0u8; 8];
    sender_id.copy_from_slice(&hex(sender));
    let mut p = packet::Packet::new(packet_type, ttl, sender_id, hex(payload));
    p.version = version;
    p.timestamp = timestamp;
    p
}

#[test]
fn v1_packet_matches_canonical_unpadded_bytes() {
    let p = packet(
        1,
        MessageType::Message as u8,
        7,
        0x0102030405060708,
        "1011121314151617",
        "aabbcc",
    );

    let encoded = packet::encode(&p).unwrap();
    assert_eq!(
        hex("01020701020304050607080000031011121314151617aabbcc"),
        encoded
    );
    assert_eq!(p, packet::decode(&encoded).unwrap());
}

#[test]
fn v2_routed_signed_packet_matches_canonical_section_order() {
    let signature = [0x5au8; 64];
    let mut recipient = [0u8; 8];
    recipient.copy_from_slice(&hex("1112131415161718"));
    let mut hop1 = [0u8; 8];
    hop1.copy_from_slice(&hex("2122232425262728"));
    let mut hop2 = [0u8; 8];
    hop2.copy_from_slice(&hex("3132333435363738"));

    let mut p = packet(
        2,
        MessageType::NoiseEncrypted as u8,
        5,
        42,
        "0102030405060708",
        "dead",
    );
    p.recipient_id = Some(recipient);
    p.signature = Some(signature);
    p.route = Some(vec![hop1, hop2]);

    let encoded = packet::encode(&p).unwrap();
    let mut expected = hex(concat!(
        "021105000000000000002a0b00000002",
        "0102030405060708",
        "1112131415161718",
        "02",
        "2122232425262728",
        "3132333435363738",
        "dead",
    ));
    expected.extend_from_slice(&signature);
    assert_eq!(expected, encoded);
    assert_eq!(p, packet::decode(&encoded).unwrap());
}

#[test]
fn signing_preimage_uses_ttl_zero_and_omits_signature() {
    let mut p = packet(
        2,
        MessageType::Message as u8,
        7,
        99,
        "0102030405060708",
        "abcd",
    );
    p.signature = Some([0x11u8; 64]);
    p.recipient_id = Some([0x22u8; 8]);

    let signing = p.encode_for_signing().unwrap();
    let decoded = packet::decode(&signing).unwrap();
    assert_eq!(0, decoded.ttl);
    assert_eq!(None, decoded.signature);
    // Everything else is preserved in the preimage.
    assert_eq!(p.version, decoded.version);
    assert_eq!(p.packet_type, decoded.packet_type);
    assert_eq!(p.timestamp, decoded.timestamp);
    assert_eq!(p.recipient_id, decoded.recipient_id);
    assert_eq!(p.payload, decoded.payload);
}

#[test]
fn decoder_rejects_unsupported_version() {
    let mut bytes = hex("01020701020304050607080000031011121314151617aabbcc");
    bytes[0] = 3; // version 3 is not defined
    assert!(packet::decode(&bytes).is_none());
}

#[test]
fn decoder_rejects_every_truncation() {
    let p = packet(
        2,
        MessageType::Message as u8,
        7,
        0x0102030405060708,
        "1011121314151617",
        "aabbcc",
    );
    let wire = packet::encode(&p).unwrap();

    for length in 0..wire.len() {
        assert!(
            packet::decode(&wire[..length]).is_none(),
            "accepted truncated packet of {length}/{} bytes",
            wire.len()
        );
    }
}

#[test]
fn decoder_rejects_oversized_declared_payload() {
    let p = packet(
        1,
        MessageType::Message as u8,
        7,
        1,
        "0102030405060708",
        "aabb",
    );
    let mut wire = packet::encode(&p).unwrap();
    // v1 length field is bytes 12..14 (0-indexed after version/type/ttl/ts/flags).
    let declared = (packet::MAX_PAYLOAD_LENGTH as u32 + 1).min(0xFFFF) as u16;
    wire[12..14].copy_from_slice(&declared.to_be_bytes());
    assert!(packet::decode(&wire).is_none());
}

#[test]
fn decoder_ignores_route_flag_on_v1_packets() {
    let p = packet(
        1,
        MessageType::Message as u8,
        7,
        1,
        "0102030405060708",
        "aabb",
    );
    let mut wire = packet::encode(&p).unwrap();
    // Set HAS_ROUTE (0x08) on a v1 packet; decode must ignore it.
    wire[11] |= packet::FLAG_HAS_ROUTE;
    let decoded = packet::decode(&wire).unwrap();
    assert_eq!(None, decoded.route);
}

#[test]
fn reencode_reuses_originator_compressed_bytes() {
    // Fake DEFLATE: the decompressor maps "aa" -> 3 bytes of 0xAB. The decoder
    // must retain the originator's compressed bytes so a relay re-encode does
    // not substitute its own encoding.
    let payload = vec![0xABu8; 3];
    let compressed = vec![0xAAu8; 2];

    let mut wire = Vec::new();
    wire.push(1u8); // version
    wire.push(MessageType::Message as u8);
    wire.push(7u8); // ttl
    wire.extend_from_slice(&1u64.to_be_bytes());
    wire.push(packet::FLAG_IS_COMPRESSED);
    // payloadLength = 2-byte original size + 2 compressed bytes = 4
    wire.extend_from_slice(&4u16.to_be_bytes());
    wire.extend_from_slice(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]);
    wire.extend_from_slice(&3u16.to_be_bytes()); // original size
    wire.extend_from_slice(&compressed);

    let decoded = packet::decode_with(&wire, &|data, size| {
        if data == compressed.as_slice() && size == 3 {
            Some(payload.clone())
        } else {
            None
        }
    })
    .unwrap();
    assert_eq!(payload, decoded.payload);

    let reencoded = packet::encode(&decoded).unwrap();
    assert_eq!(
        wire, reencoded,
        "relay re-encode must reuse originator bytes"
    );
}

#[test]
fn real_compression_round_trips_through_packet_codec() {
    // A compressible payload (100+ bytes, low entropy) must encode with the
    // IS_COMPRESSED flag set, decode back to the exact payload, and re-encode
    // byte-identically via the wire-payload reuse path (relay parity).
    let mut payload = Vec::new();
    for i in 0..300u32 {
        payload.extend_from_slice(format!("mesh-{i:03}-").as_bytes());
    }
    assert!(bitchat_wire::compression::should_compress(&payload));

    let mut p = packet(
        1,
        MessageType::Message as u8,
        7,
        4242,
        "0102030405060708",
        "",
    );
    p.payload = payload.clone();

    let encoded = packet::encode(&p).unwrap();
    // Sanity: compressed flag set, frame smaller than the raw payload.
    assert_eq!(
        packet::FLAG_IS_COMPRESSED,
        encoded[11] & packet::FLAG_IS_COMPRESSED
    );
    assert!(encoded.len() < 14 + 8 + payload.len());

    let decoded = packet::decode(&encoded).unwrap();
    assert_eq!(payload, decoded.payload);
    let reencoded = packet::encode(&decoded).unwrap();
    assert_eq!(
        encoded, reencoded,
        "relay re-encode must reuse originator bytes"
    );
}

#[test]
fn compressed_guards_fail_closed() {
    let mk = |original_size: u16, compressed: &[u8]| -> Vec<u8> {
        let mut wire = Vec::new();
        wire.push(1u8);
        wire.push(MessageType::Message as u8);
        wire.push(7u8);
        wire.extend_from_slice(&1u64.to_be_bytes());
        wire.push(packet::FLAG_IS_COMPRESSED);
        wire.extend_from_slice(&((2 + compressed.len()) as u16).to_be_bytes());
        wire.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        wire.extend_from_slice(&original_size.to_be_bytes());
        wire.extend_from_slice(compressed);
        wire
    };
    let ok = |_: &[u8], _: usize| Some(vec![0xABu8; 3]);

    // Original size 0 is outside 1..=MAX.
    assert!(packet::decode_with(&mk(0, &[0xAA, 0xAA]), &ok).is_none());
    // Mismatched expanded size.
    assert!(packet::decode_with(&mk(4, &[0xAA, 0xAA]), &ok).is_none());
    // Compression ratio above 50 000:1.
    assert!(packet::decode_with(&mk(u16::MAX, &[0xAA, 0xAA]), &ok).is_none());
}
