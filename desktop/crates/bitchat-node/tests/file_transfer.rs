//! File transfer over the fragmentation path: a maximal-size FilePacket TLV
//! must fragment within the mesh v1 caps (256 fragments) and reassemble to
//! the identical packet, otherwise phone peers would reject our media.

use bitchat_node::link::MAX_WIRE_FILE_BYTES;
use bitchat_wire::file_packet::FilePacket;
use bitchat_wire::packet::{self, MessageType, Packet};
use bitchat_wire::reassembly::{create_fragments, Reassembler, Reassembly, MAX_FRAGMENTS_PER_ID};

/// Deterministic incompressible bytes (packet::encode would otherwise
/// compress a repetitive payload down below the fragmentation threshold).
fn pseudo_random(len: usize) -> Vec<u8> {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        out.extend_from_slice(&state.wrapping_mul(0x2545_F491_4F6C_DD1D).to_be_bytes());
    }
    out.truncate(len);
    out
}

#[test]
fn max_file_survives_fragment_roundtrip() {
    let content = pseudo_random(MAX_WIRE_FILE_BYTES - 64);
    let file = FilePacket::new("photo.jpg", content.len() as i64, "image/jpeg", content);
    let tlv = file.encode().expect("encode");
    assert!(tlv.len() <= MAX_WIRE_FILE_BYTES);

    let mut packet = Packet::new(
        MessageType::FileTransfer as u8,
        7,
        [0x2A; 8],
        tlv.clone(),
    );
    packet.version = 2;
    packet.recipient_id = Some([0xFF; 8]);
    packet.timestamp = 1_700_000_000_000;
    packet.signature = Some([0x55; 64]);

    let fragments = create_fragments(&packet, MAX_FRAGMENTS_PER_ID as usize);
    assert!(!fragments.is_empty(), "must be fragmentable");
    assert!(
        fragments.len() <= MAX_FRAGMENTS_PER_ID as usize,
        "{} fragments exceed the mesh v1 cap",
        fragments.len()
    );

    let mut reassembler = Reassembler::new();
    let mut completed = None;
    for fragment in &fragments {
        match reassembler.handle_fragment(&fragment.payload, 1_000) {
            Reassembly::Pending => {}
            Reassembly::Complete(packet) => completed = Some(packet),
            Reassembly::Rejected => panic!("fragment rejected"),
        }
    }
    let reassembled = completed.expect("reassembled");
    assert_eq!(reassembled.packet_type, packet.packet_type);
    assert_eq!(reassembled.payload, tlv);
    assert_eq!(reassembled.signature, packet.signature);

    let decoded = FilePacket::decode(&reassembled.payload).expect("decode");
    assert_eq!(decoded.file_name, "photo.jpg");
    assert_eq!(decoded.mime_type, "image/jpeg");
    assert_eq!(decoded.content, file.content);
}

#[test]
fn oversized_file_is_unfragmentable() {
    let mut packet = Packet::new(
        MessageType::FileTransfer as u8,
        7,
        [0x2A; 8],
        pseudo_random(2_000_000),
    );
    packet.version = 2;
    packet.timestamp = 1;
    packet.signature = Some([0x55; 64]);
    // The packet itself encodes (version 2 lengths), but no fragment plan
    // stays within the mesh v1 caps — this is what protects the mesh.
    assert!(packet::encode(&packet).is_some());
    assert!(create_fragments(&packet, MAX_FRAGMENTS_PER_ID as usize).is_empty());
}
