//! Replicates the phone's ANNOUNCE acceptance chain
//! (`AnnouncementIdentityValidator.verify` + `MessageHandler.handleAnnounce`)
//! against the exact bytes the node puts on the BLE characteristic.

use bitchat_wire::announce::IdentityAnnouncement;
use bitchat_wire::noise::derive_peer_id;
use bitchat_wire::packet::{self, MessageType, Packet};

const MAX_TTL: u8 = 7;

fn to_hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[test]
fn signed_announce_passes_phone_validation_chain() {
    let id = bitchat_node::NodeIdentity::generate().expect("identity generates");
    let peer_id = id.peer_id.clone();

    // Build exactly like MeshLink::send_announce_to.
    let announce = IdentityAnnouncement {
        nickname: "bitchatd".to_string(),
        noise_public_key: id.noise_public_key.to_vec(),
        signing_public_key: id.signing.public_key().to_vec(),
        capabilities: Some(bitchat_wire::announce::PeerCapabilities(0)),
        unknown_tlvs: Vec::new(),
    };
    let payload = announce.encode().expect("announce encodes");
    let mut pkt = Packet::new(
        MessageType::Announce as u8,
        MAX_TTL,
        from_hex(&peer_id).try_into().expect("8 bytes"),
        payload,
    );
    pkt.timestamp = now_ms();
    pkt.signature = id.signing.sign_packet(&pkt);

    // Bytes as they go on the wire (ANNOUNCE is not padded over BLE).
    let frame = packet::encode(&pkt).expect("frame encodes");
    let mut bytes = frame.clone();
    if bitchat_ble::frame::should_pad_for_ble(pkt.packet_type) {
        let target = bitchat_wire::padding::optimal_block_size(bytes.len());
        bytes = bitchat_wire::padding::pad(&bytes, target);
    }

    // --- phone side: BluetoothGattServerManager does fromBinaryData(value) ---
    let decoded = packet::decode(&bytes).expect("phone decodes the frame");
    assert_eq!(decoded.packet_type, MessageType::Announce as u8);

    // --- AnnouncementIdentityValidator.verify ---
    // 1. clock skew
    let now = now_ms() as i64;
    let skew = (now - decoded.timestamp as i64).abs();
    assert!(skew <= 10 * 60 * 1000, "clock skew {skew}ms");

    // 2. announcement payload decodes, required TLVs present
    let parsed = IdentityAnnouncement::decode(&decoded.payload)
        .expect("IdentityAnnouncement.decode succeeds");
    assert_eq!(parsed.signing_public_key.len(), 32);
    assert_eq!(parsed.nickname, "bitchatd");

    // 3. peer id binding: sha256(noise key)[..8] == packet sender
    let derived = derive_peer_id(
        parsed
            .noise_public_key
            .as_slice()
            .try_into()
            .expect("32 bytes"),
    );
    assert_eq!(to_hex(&decoded.sender_id), derived);
    assert_eq!(peer_id, derived);

    // 4. signature over the canonical re-encoding (TTL=0, no signature,
    //    originator payload bytes preserved via wire_payload)
    let canonical = decoded.encode_for_signing().expect("canonical preimage");
    let sig = decoded.signature.expect("signature present");
    let key: [u8; 32] = parsed
        .signing_public_key
        .as_slice()
        .try_into()
        .expect("32 bytes");
    assert!(
        bitchat_wire::identity::verify(&key, &canonical, &sig),
        "Ed25519 signature verifies over canonical preimage"
    );
}
