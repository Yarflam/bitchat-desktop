//! Golden-vector tests for `AuthenticatedPeerState` (Noise payload 0x21),
//! byte-exact port of `AuthenticatedPeerState.kt`.

use bitchat_wire::announce::PeerCapabilities;
use bitchat_wire::noise_payload::AuthenticatedPeerState;

#[test]
fn encode_matches_kotlin_layout() {
    let signing = [0xABu8; 32];
    let state = AuthenticatedPeerState::new(PeerCapabilities(0), signing);
    let encoded = state.encode();
    // [0x01][0x01][0x01][0x00][0x02][0x20][32 bytes]
    assert_eq!(encoded.len(), 1 + 2 + 1 + 2 + 32);
    assert_eq!(encoded[0], 0x01, "version");
    assert_eq!(encoded[1], 0x01, "capabilities tlv");
    assert_eq!(encoded[2], 0x01, "capabilities length");
    assert_eq!(encoded[3], 0x00, "capabilities value (NONE)");
    assert_eq!(encoded[4], 0x02, "signing key tlv");
    assert_eq!(encoded[5], 32, "signing key length");
    assert_eq!(&encoded[6..], &signing[..]);
}

#[test]
fn encode_minimal_le_capabilities() {
    let state = AuthenticatedPeerState::new(PeerCapabilities(0x0102), [7u8; 32]);
    let encoded = state.encode();
    assert_eq!(&encoded[1..5], &[0x01, 0x02, 0x02, 0x01]);
    assert_eq!(AuthenticatedPeerState::decode(&encoded), Some(state));
}

#[test]
fn decode_skips_unknown_tlvs() {
    let state = AuthenticatedPeerState::new(PeerCapabilities(1), [9u8; 32]);
    let mut bytes = state.encode();
    // Insert unknown TLV [0x7F len 2 AA BB] after the version byte.
    bytes.splice(1..1, [0x7F, 0x02, 0xAA, 0xBB]);
    assert_eq!(AuthenticatedPeerState::decode(&bytes), Some(state));
}

#[test]
fn decode_rejects_duplicates_and_bad_version() {
    let state = AuthenticatedPeerState::new(PeerCapabilities(1), [3u8; 32]);
    let encoded = state.encode();

    let mut dup_caps = encoded.clone();
    dup_caps.splice(1..1, [0x01u8, 0x01, 0x05]);
    assert_eq!(AuthenticatedPeerState::decode(&dup_caps), None);

    let mut bad_version = encoded.clone();
    bad_version[0] = 0x02;
    assert_eq!(AuthenticatedPeerState::decode(&bad_version), None);

    let mut truncated = encoded.clone();
    truncated.pop();
    assert_eq!(AuthenticatedPeerState::decode(&truncated), None);
}

#[test]
fn decode_rejects_non_minimal_capabilities() {
    let state = AuthenticatedPeerState::new(PeerCapabilities(1), [3u8; 32]);
    let encoded = state.encode();
    // capabilities TLV with a trailing zero byte is not the minimal encoding
    let mut padded = encoded.clone();
    padded[2] = 0x02;
    padded.splice(4..4, [0x00]);
    assert_eq!(AuthenticatedPeerState::decode(&padded), None);
}
