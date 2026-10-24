//! GATT link framing, mirroring the Android BLE transport.
//!
//! Deployed semantics (from `BluetoothGatt{Client,Server}Manager.kt` and
//! `BLEPacketPaddingPolicy.kt`):
//! - One characteristic write or notification carries exactly ONE complete
//!   bitchat frame (the protocol's own FRAGMENT type handles large payloads,
//!   keeping every frame within the ATT budget).
//! - Only NOISE_HANDSHAKE and NOISE_ENCRYPTED frames are padded
//!   (PKCS#7 to the optimal block size); all other types go out unpadded.
//! - Client writes use WRITE_TYPE_NO_RESPONSE.
//! - The advertisement carries the 8-byte peer ID in the service data of the
//!   BitChat service UUID, so scanners can filter without connecting.

use bitchat_wire::packet::{self, Packet};
use bitchat_wire::padding;
use uuid::Uuid;

/// Mirror of `AppConstants.Mesh.Gatt.SERVICE_UUID`.
pub const SERVICE_UUID: Uuid = Uuid::from_u128(0xf47b5e2d4a9e4c5a_9b3f8e1d2c3a4b5c);
/// Mirror of `AppConstants.Mesh.Gatt.CHARACTERISTIC_UUID`.
pub const CHARACTERISTIC_UUID: Uuid = Uuid::from_u128(0xa1b2c3d4e5f64a5b_8c9d0e1f2a3b4c5d);
/// Mirror of `AppConstants.Mesh.Gatt.DESCRIPTOR_UUID` (CCCD).
pub const CCCD_UUID: Uuid = Uuid::from_u128(0x0000290200001000_800000805f9b34fb);

/// ATT payload for the MTU Android negotiates (requestMtu(517)).
pub const DEFAULT_ATT_MTU: usize = 517;
/// Usable bytes per ATT packet: MTU minus the 3-byte ATT header.
pub const ATT_HEADER_SIZE: usize = 3;

/// Error type for link-level frame operations (fail-closed, no panics).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("encoded frame of {size} bytes exceeds the {budget}-byte ATT budget")]
    FrameTooLarge { size: usize, budget: usize },
}

/// Mirror of `BLEPacketPaddingPolicy.shouldPadForBLE`: only Noise frames are
/// padded over BLE.
pub fn should_pad_for_ble(packet_type: u8) -> bool {
    matches!(
        bitchat_wire::MessageType::from_value(packet_type),
        Some(bitchat_wire::MessageType::NoiseHandshake)
            | Some(bitchat_wire::MessageType::NoiseEncrypted)
    )
}

/// Encode a packet into one BLE frame: encode, then pad Noise frames per the
/// policy. Fails closed when the frame does not fit the negotiated ATT MTU.
pub fn encode_frame(packet: &Packet, att_mtu: usize) -> Result<Vec<u8>, FrameError> {
    let mut frame = packet::encode(packet).ok_or(FrameError::FrameTooLarge {
        size: usize::MAX,
        budget: att_mtu.saturating_sub(ATT_HEADER_SIZE),
    })?;
    if should_pad_for_ble(packet.packet_type) {
        let target = padding::optimal_block_size(frame.len());
        frame = padding::pad(&frame, target);
    }
    let budget = att_mtu.saturating_sub(ATT_HEADER_SIZE);
    if frame.len() > budget {
        return Err(FrameError::FrameTooLarge {
            size: frame.len(),
            budget,
        });
    }
    Ok(frame)
}

/// Decode one BLE frame back into a packet. Tolerates padded and unpadded
/// input exactly like the Android decoders.
pub fn decode_frame(frame: &[u8]) -> Option<Packet> {
    packet::decode(frame)
}

/// Extract the advertised 8-byte peer ID from the service-data map of an
/// advertisement. Mirrors the Android scanner, which reads the BitChat
/// service data as the peer's wire ID bytes.
pub fn advertised_peer_id(
    service_data: &std::collections::HashMap<Uuid, Vec<u8>>,
) -> Option<[u8; 8]> {
    let bytes = service_data.get(&SERVICE_UUID)?;
    bytes.first_chunk::<8>().copied()
}

/// Wire peer ID (16 lowercase hex chars) of a packet sender, matching the
/// Android conversion of `senderID` bytes.
pub fn peer_id_from_sender(sender_id: &[u8; 8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(16);
    for &b in sender_id {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitchat_wire::{MessageType, Packet};

    fn pkt(packet_type: u8, payload_size: usize) -> Packet {
        let mut p = Packet::new(packet_type, 7, [0x42; 8], vec![0xAB; payload_size]);
        p.timestamp = 99;
        p
    }

    #[test]
    fn padding_policy_only_covers_noise_frames() {
        assert!(should_pad_for_ble(MessageType::NoiseHandshake as u8));
        assert!(should_pad_for_ble(MessageType::NoiseEncrypted as u8));
        assert!(!should_pad_for_ble(MessageType::Announce as u8));
        assert!(!should_pad_for_ble(MessageType::Message as u8));
        assert!(!should_pad_for_ble(MessageType::Fragment as u8));
        assert!(!should_pad_for_ble(0x77)); // unknown types are not padded
    }

    #[test]
    fn noise_frames_are_padded_to_block_size() {
        let p = pkt(MessageType::NoiseEncrypted as u8, 20);
        let frame = encode_frame(&p, DEFAULT_ATT_MTU).unwrap();
        assert!(frame.len() > 20);
        assert!(matches!(frame.len(), 256 | 512 | 1024 | 2048));
        // The padding is transparent to the decoder.
        assert_eq!(p, decode_frame(&frame).unwrap());
    }

    #[test]
    fn plain_frames_stay_unpadded() {
        let p = pkt(MessageType::Message as u8, 20);
        let frame = encode_frame(&p, DEFAULT_ATT_MTU).unwrap();
        assert_eq!(packet::encode(&p).unwrap(), frame);
        assert_eq!(p, decode_frame(&frame).unwrap());
    }

    #[test]
    fn oversized_frames_fail_closed() {
        // 600 bytes of incompressible payload exceeds the 514-byte budget
        // even before fragmentation planning; the link layer must refuse.
        let mut state = 0x9E3779B97F4A7C15u64;
        let payload: Vec<u8> = (0..600)
            .map(|_| {
                state = state.wrapping_add(0x9E3779B97F4A7C15);
                let mut z = state;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
                z ^= z >> 31;
                (z >> 56) as u8
            })
            .collect();
        let p = pkt(MessageType::Message as u8, 0);
        let mut p = p;
        p.payload = payload;
        match encode_frame(&p, DEFAULT_ATT_MTU) {
            Err(FrameError::FrameTooLarge { .. }) => {}
            other => panic!("expected FrameTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn fragmentation_keeps_frames_within_att_budget() {
        // A packet just above the protocol threshold fragments into frames
        // that each fit the ATT budget, so the link layer accepts them.
        use bitchat_wire::reassembly::create_fragments;
        let mut state = 0x9E3779B97F4A7C15u64;
        let payload: Vec<u8> = (0..700)
            .map(|_| {
                state = state.wrapping_add(0x9E3779B97F4A7C15);
                let mut z = state;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
                z ^= z >> 31;
                (z >> 56) as u8
            })
            .collect();
        let mut p = pkt(MessageType::Message as u8, 0);
        p.payload = payload;
        let fragments = create_fragments(&p, 0xFFFF);
        assert!(fragments.len() > 1);
        for f in &fragments {
            let frame = encode_frame(f, DEFAULT_ATT_MTU)
                .unwrap_or_else(|e| panic!("fragment frame must fit ATT budget: {e}"));
            assert!(frame.len() <= DEFAULT_ATT_MTU - ATT_HEADER_SIZE);
        }
    }

    #[test]
    fn advertised_peer_id_parsing() {
        let mut sd = std::collections::HashMap::new();
        assert_eq!(None, advertised_peer_id(&sd));
        sd.insert(SERVICE_UUID, vec![1, 2, 3]); // too short
        assert_eq!(None, advertised_peer_id(&sd));
        sd.insert(SERVICE_UUID, vec![0xDE, 0xAD, 0xBE, 0xEF, 1, 2, 3, 4]);
        assert_eq!(
            Some([0xDE, 0xAD, 0xBE, 0xEF, 1, 2, 3, 4]),
            advertised_peer_id(&sd)
        );
        // Extra bytes beyond the wire ID are tolerated (first 8 win).
        sd.insert(
            SERVICE_UUID,
            vec![0xDE, 0xAD, 0xBE, 0xEF, 1, 2, 3, 4, 0xAA, 0xBB],
        );
        assert_eq!(
            Some([0xDE, 0xAD, 0xBE, 0xEF, 1, 2, 3, 4]),
            advertised_peer_id(&sd)
        );
    }

    #[test]
    fn sender_id_to_wire_peer_id() {
        assert_eq!(
            "deadbeef01020304",
            peer_id_from_sender(&[0xDE, 0xAD, 0xBE, 0xEF, 1, 2, 3, 4])
        );
        assert_eq!("0000000000000000", peer_id_from_sender(&[0; 8]));
    }
}
