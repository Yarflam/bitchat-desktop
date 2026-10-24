//! Golden wire vectors for TLV payload formats.
//!
//! Literal-byte assertions ported from
//! `ClientRewriteWireContractTest.kt`: private messages, the Noise envelope,
//! the 13-byte fragment header, the sync request TLVs, identity announcements,
//! and the deployed mixed-width file transfer format. Rejection cases mirror
//! the `TLV decoders reject missing required fields and truncated values`
//! test.

use bitchat_wire::announce::{IdentityAnnouncement, PeerCapabilities};
use bitchat_wire::file_packet::FilePacket;
use bitchat_wire::fragment::FragmentPayload;
use bitchat_wire::noise_payload::{NoisePayload, NoisePayloadType, PrivateMessagePacket};
use bitchat_wire::sync::RequestSyncPacket;

fn hex(value: &str) -> Vec<u8> {
    assert!(value.len().is_multiple_of(2));
    value
        .as_bytes()
        .chunks(2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn private_message_and_noise_envelopes_match_deployed_type_bytes() {
    let private_message = PrivateMessagePacket::new("m1", "hi");
    let private_message_bytes = hex("00026d3101026869");

    assert_eq!(private_message_bytes, private_message.encode().unwrap());
    assert_eq!(
        private_message,
        PrivateMessagePacket::decode(&private_message_bytes).unwrap()
    );
    assert_eq!(
        hex("0100026d3101026869"),
        NoisePayload::new(NoisePayloadType::PrivateMessage, private_message_bytes).encode()
    );
    // Prerelease iOS 0x09 alias decodes as FILE_TRANSFER but re-encodes canonically.
    assert_eq!(
        NoisePayloadType::FileTransfer,
        NoisePayload::decode(&hex("09cafe")).unwrap().payload_type
    );
    assert_eq!(
        hex("20cafe"),
        NoisePayload::decode(&hex("09cafe")).unwrap().encode()
    );
}

#[test]
fn fragment_payload_matches_the_thirteen_byte_header() {
    let fragment_id: [u8; 8] = hex("0001020304050607").try_into().unwrap();
    let fragment = FragmentPayload {
        fragment_id,
        index: 1,
        total: 3,
        original_type: 0x02,
        data: hex("aabb"),
    };
    let wire = hex("00010203040506070001000302aabb");

    assert_eq!(wire, fragment.encode());
    assert_eq!(fragment, FragmentPayload::decode(&wire).unwrap());
    assert!(fragment.is_valid());
}

#[test]
fn sync_request_matches_canonical_tlv_bytes_and_skips_extensions() {
    let request = RequestSyncPacket::new(19, 0x01020304, hex("aabb"));
    let wire = hex("0100011302000401020304030002aabb");

    assert_eq!(wire, request.encode());
    assert_eq!(request, RequestSyncPacket::decode(&wire).unwrap());

    let mut with_extension = hex("7f0002cafe");
    with_extension.extend_from_slice(&wire);
    assert_eq!(request, RequestSyncPacket::decode(&with_extension).unwrap());
}

#[test]
fn identity_announcement_matches_canonical_tlv_order_and_preserves_extensions() {
    let announcement = IdentityAnnouncement {
        nickname: "bob".to_string(),
        noise_public_key: vec![0x11; 32],
        signing_public_key: vec![0x22; 32],
        capabilities: Some(PeerCapabilities::PRIVATE_MEDIA),
        unknown_tlvs: vec![bitchat_wire::announce::UnknownTlv {
            tlv_type: 0x7f,
            value: hex("cafe"),
        }],
    };
    let mut expected = hex("0103626f620220");
    expected.extend_from_slice(&[0x11; 32]);
    expected.extend_from_slice(&hex("0320"));
    expected.extend_from_slice(&[0x22; 32]);
    expected.extend_from_slice(&hex("050200017f02cafe"));

    let encoded = announcement.encode().unwrap();
    assert_eq!(expected, encoded);
    assert_eq!(
        announcement,
        IdentityAnnouncement::decode(&encoded).unwrap()
    );
}

#[test]
fn file_transfer_matches_deployed_mixed_width_tlv_vector() {
    let packet = FilePacket::new("a", 2, "m", hex("dead"));
    let wire = hex("01000161020004000000020300016d0400000002dead");

    assert_eq!(wire, packet.encode().unwrap());
    assert_eq!(packet, FilePacket::decode(&wire).unwrap());
}

#[test]
fn tlv_decoders_reject_missing_fields_and_truncated_values() {
    assert!(PrivateMessagePacket::decode(&hex("00026d31")).is_none());
    assert!(PrivateMessagePacket::decode(&hex("00026d3101036869")).is_none());
    assert!(RequestSyncPacket::decode(&hex("0100011302000401020304")).is_none());
    assert!(IdentityAnnouncement::decode(&hex("0103626f62022011")).is_none());
    assert!(FilePacket::decode(&hex("010001610400000002de")).is_none());
    assert!(FragmentPayload::decode(&vec![0u8; 12]).is_none());
}

#[test]
fn fragment_split_respects_max_data_and_reassembles() {
    let payload: Vec<u8> = (0..=255u8).collect();
    let fragment_id = [0x42u8; 8];
    let fragments = FragmentPayload::split(fragment_id, 0x02, &payload, 100);
    assert_eq!(3, fragments.len());
    assert!(fragments.iter().all(|f| f.total == 3));
    assert_eq!(2, fragments[2].index);

    let mut reassembled = Vec::new();
    for f in &fragments {
        assert!(f.is_valid());
        reassembled.extend_from_slice(&f.data);
    }
    assert_eq!(payload, reassembled);
    for f in fragments {
        assert_eq!(f, FragmentPayload::decode(&f.encode()).unwrap());
    }
}
