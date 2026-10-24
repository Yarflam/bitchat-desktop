//! Binary packet framing, 100% compatible with the Android/iOS clients.
//!
//! Header (14 bytes for v1, 16 bytes for v2):
//! - Version: 1 byte (1 or 2; anything else is rejected)
//! - Type: 1 byte
//! - TTL: 1 byte
//! - Timestamp: 8 bytes (u64, big-endian, epoch millis)
//! - Flags: 1 byte (bit 0: hasRecipient, bit 1: hasSignature, bit 2: isCompressed,
//!   bit 3: hasRoute, v2+ only)
//! - PayloadLength: 2 bytes (v1) / 4 bytes (v2), big-endian; covers the payload
//!   plus the original-size field when compressed
//!
//! Sections after the header, in order:
//! 1. SenderID: 8 bytes (zero-padded)
//! 2. RecipientID: 8 bytes, if hasRecipient
//! 3. Route: v2+ only, if hasRoute: 1-byte hop count then N x 8-byte peer IDs
//! 4. Payload; if compressed, original size first (2 bytes v1 / 4 bytes v2),
//!    then raw-DEFLATE bytes
//! 5. Signature: 64 bytes, if hasSignature

use std::rc::Rc;

pub const HEADER_SIZE_V1: usize = 14;
pub const HEADER_SIZE_V2: usize = 16;
pub const SENDER_ID_SIZE: usize = 8;
pub const RECIPIENT_ID_SIZE: usize = 8;
pub const SIGNATURE_SIZE: usize = 64;

/// Mirror of `AppConstants.Protocol.MAX_PAYLOAD_LENGTH`.
pub const MAX_PAYLOAD_LENGTH: usize = 10_485_760;

/// Compression-bomb guard, mirror of the ratio check in `BinaryProtocol`.
pub const MAX_COMPRESSION_RATIO: f64 = 50_000.0;

pub const FLAG_HAS_RECIPIENT: u8 = 0x01;
pub const FLAG_HAS_SIGNATURE: u8 = 0x02;
pub const FLAG_IS_COMPRESSED: u8 = 0x04;
pub const FLAG_HAS_ROUTE: u8 = 0x08;

/// TTL normalized for signing; mirrors `AppConstants.SYNC_TTL_HOPS`.
pub const SIGNING_TTL: u8 = 0;

/// Packet message types; exact same values as the Android `MessageType` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    Announce = 0x01,
    Message = 0x02,
    Leave = 0x03,
    NoiseHandshake = 0x10,
    NoiseEncrypted = 0x11,
    Fragment = 0x20,
    RequestSync = 0x21,
    FileTransfer = 0x22,
    VoiceFrame = 0x29,
}

impl MessageType {
    pub fn from_value(value: u8) -> Option<Self> {
        Some(match value {
            0x01 => Self::Announce,
            0x02 => Self::Message,
            0x03 => Self::Leave,
            0x10 => Self::NoiseHandshake,
            0x11 => Self::NoiseEncrypted,
            0x20 => Self::Fragment,
            0x21 => Self::RequestSync,
            0x22 => Self::FileTransfer,
            0x29 => Self::VoiceFrame,
            _ => return None,
        })
    }
}

/// Payload as it arrived on the wire, retained by `decode`.
///
/// DEFLATE output is not canonical across encoders (java.util.zip vs other
/// implementations), so a relay that re-encodes on TTL decrement must reuse the
/// originator's bytes instead of substituting its own encoding.
/// `for_payload` ties the bytes to the payload they decode to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WirePayload {
    pub bytes: Rc<[u8]>,
    pub compressed: bool,
    pub for_payload: Rc<[u8]>,
}

/// A decoded (or to-be-encoded) BitChat packet.
///
/// Equality ignores `wire_payload`, mirroring the Kotlin packet where the wire
/// preimage is excluded from `equals`/`hashCode`.
#[derive(Debug, Clone, Eq)]
pub struct Packet {
    pub version: u8,
    pub packet_type: u8,
    pub sender_id: [u8; SENDER_ID_SIZE],
    pub recipient_id: Option<[u8; RECIPIENT_ID_SIZE]>,
    pub timestamp: u64,
    pub payload: Vec<u8>,
    pub signature: Option<[u8; SIGNATURE_SIZE]>,
    pub ttl: u8,
    /// Ordered list of peer IDs, excluding sender and final recipient.
    /// Empty routes canonicalize to `None`, as on Android.
    pub route: Option<Vec<[u8; SENDER_ID_SIZE]>>,
    /// Set by `decode`; not part of packet identity.
    pub wire_payload: Option<WirePayload>,
}

impl PartialEq for Packet {
    fn eq(&self, other: &Self) -> bool {
        self.version == other.version
            && self.packet_type == other.packet_type
            && self.sender_id == other.sender_id
            && self.recipient_id == other.recipient_id
            && self.timestamp == other.timestamp
            && self.payload == other.payload
            && self.signature == other.signature
            && self.ttl == other.ttl
            && self.route == other.route
    }
}

impl Packet {
    pub fn new(
        packet_type: u8,
        ttl: u8,
        sender_id: [u8; SENDER_ID_SIZE],
        payload: Vec<u8>,
    ) -> Self {
        Packet {
            version: 1,
            packet_type,
            sender_id,
            recipient_id: None,
            timestamp: 0,
            payload,
            signature: None,
            ttl,
            route: None,
            wire_payload: None,
        }
    }

    /// Binary representation for signing: signature omitted, TTL normalized
    /// to 0 so relay TTL changes cannot invalidate the signature.
    pub fn encode_for_signing(&self) -> Option<Vec<u8>> {
        let mut unsigned = self.clone();
        unsigned.signature = None;
        unsigned.ttl = SIGNING_TTL;
        encode(&unsigned)
    }
}

fn header_size(version: u8) -> usize {
    match version {
        1 => HEADER_SIZE_V1,
        _ => HEADER_SIZE_V2,
    }
}

/// Encode a packet without padding. Returns `None` on any size violation,
/// mirroring the Kotlin `encode` null returns (fail-closed).
pub fn encode(packet: &Packet) -> Option<Vec<u8>> {
    if packet.payload.len() > MAX_PAYLOAD_LENGTH {
        return None;
    }

    // Re-encode of a decoded packet: reuse the originator's wire bytes.
    let mut payload: &[u8] = &packet.payload;
    let owned_payload: Vec<u8>;
    let mut original_payload_size: Option<usize> = None;
    let mut is_compressed = false;
    if let Some(wire) = &packet.wire_payload {
        if wire.for_payload.as_ref() == packet.payload.as_slice() && wire.compressed {
            payload = &wire.bytes;
            original_payload_size = Some(packet.payload.len());
            is_compressed = true;
        }
    }
    // Compress only when the heuristic agrees and the result is smaller;
    // DEFLATE output is not canonical across encoders, so cross-client
    // parity lives in the decode direction (see the compression module).
    if !is_compressed && crate::compression::should_compress(&packet.payload) {
        if let Some(compressed) = crate::compression::compress(&packet.payload) {
            owned_payload = compressed;
            payload = &owned_payload;
            original_payload_size = Some(packet.payload.len());
            is_compressed = true;
        }
    }

    let version = packet.version;
    let recipient_bytes = if packet.recipient_id.is_some() {
        RECIPIENT_ID_SIZE
    } else {
        0
    };
    let signature_bytes = if packet.signature.is_some() {
        SIGNATURE_SIZE
    } else {
        0
    };
    let size_field_bytes = if is_compressed {
        if version >= 2 {
            4
        } else {
            2
        }
    } else {
        0
    };
    let payload_data_size = payload.len() + size_field_bytes;
    let has_route = packet.route.as_ref().is_some_and(|r| !r.is_empty()) && version >= 2;
    let route_bytes = if has_route {
        1 + packet.route.as_ref().unwrap().len().min(255) * SENDER_ID_SIZE
    } else {
        0
    };

    if version == 1
        && (payload_data_size > 0xFFFF || original_payload_size.is_some_and(|s| s > 0xFFFF))
    {
        return None;
    }

    let capacity = header_size(version)
        + SENDER_ID_SIZE
        + recipient_bytes
        + payload_data_size
        + signature_bytes
        + route_bytes;
    let mut buf = Vec::with_capacity(capacity);

    buf.push(version);
    buf.push(packet.packet_type);
    buf.push(packet.ttl);
    buf.extend_from_slice(&packet.timestamp.to_be_bytes());

    let mut flags = 0u8;
    if packet.recipient_id.is_some() {
        flags |= FLAG_HAS_RECIPIENT;
    }
    if packet.signature.is_some() {
        flags |= FLAG_HAS_SIGNATURE;
    }
    if is_compressed {
        flags |= FLAG_IS_COMPRESSED;
    }
    if has_route {
        flags |= FLAG_HAS_ROUTE;
    }
    buf.push(flags);

    if version >= 2 {
        buf.extend_from_slice(&(payload_data_size as u32).to_be_bytes());
    } else {
        // Bounds checked above; v1 cannot carry these sizes.
        buf.extend_from_slice(&(payload_data_size as u16).to_be_bytes());
    }

    buf.extend_from_slice(&packet.sender_id);

    if let Some(recipient) = &packet.recipient_id {
        buf.extend_from_slice(recipient);
    }

    if has_route {
        let route = packet.route.as_ref().unwrap();
        let count = route.len().min(255);
        buf.push(count as u8);
        for hop in route.iter().take(count) {
            buf.extend_from_slice(hop);
        }
    }

    if is_compressed {
        let original = original_payload_size.unwrap();
        if version >= 2 {
            buf.extend_from_slice(&(original as u32).to_be_bytes());
        } else {
            buf.extend_from_slice(&(original as u16).to_be_bytes());
        }
    }
    buf.extend_from_slice(payload);

    if let Some(signature) = &packet.signature {
        buf.extend_from_slice(signature);
    }

    Some(buf)
}

/// Decompressor seam: `(compressed_bytes, expected_expanded_size) -> expanded`.
/// Injected so tests can pin the fail-closed guards without a DEFLATE backend;
/// the production implementation wires in `flate2` (see the compression module).
pub type Decompressor<'a> = dyn Fn(&[u8], usize) -> Option<Vec<u8>> + 'a;

/// Decode a packet, first trying the bytes as-is and then unpadding.
/// Mirrors `BinaryProtocol.decode`; the production decompressor is the raw
/// DEFLATE implementation in `crate::compression`.
pub fn decode(data: &[u8]) -> Option<Packet> {
    decode_with(data, &crate::compression::decompress)
}

/// Decode with an explicit decompressor (test seam mirroring
/// `BinaryProtocol.decodeForTesting`).
pub fn decode_with(data: &[u8], decompress: &Decompressor) -> Option<Packet> {
    if let Some(packet) = decode_core(data, decompress) {
        return Some(packet);
    }
    let unpadded = crate::padding::unpad(data);
    if unpadded == data {
        return None;
    }
    decode_core(&unpadded, decompress)
}

fn read_u16_be(buf: &[u8], pos: &mut usize) -> Option<u16> {
    if *pos + 2 > buf.len() {
        return None;
    }
    let v = u16::from_be_bytes([buf[*pos], buf[*pos + 1]]);
    *pos += 2;
    Some(v)
}

fn read_u32_be(buf: &[u8], pos: &mut usize) -> Option<u32> {
    if *pos + 4 > buf.len() {
        return None;
    }
    let v = u32::from_be_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
    *pos += 4;
    Some(v)
}

fn decode_core(raw: &[u8], decompress: &Decompressor) -> Option<Packet> {
    if raw.len() < HEADER_SIZE_V1 + SENDER_ID_SIZE {
        return None;
    }

    let mut pos = 0usize;
    let version = raw[pos];
    pos += 1;
    if version != 1 && version != 2 {
        return None;
    }

    let packet_type = raw[pos];
    pos += 1;
    let ttl = raw[pos];
    pos += 1;

    if pos + 8 > raw.len() {
        return None;
    }
    let timestamp = u64::from_be_bytes(raw[pos..pos + 8].try_into().ok()?);
    pos += 8;

    let flags = raw[pos];
    pos += 1;
    let has_recipient = flags & FLAG_HAS_RECIPIENT != 0;
    let has_signature = flags & FLAG_HAS_SIGNATURE != 0;
    let is_compressed = flags & FLAG_IS_COMPRESSED != 0;
    // HAS_ROUTE is only valid for v2+ packets; ignore the flag for v1.
    let has_route = version >= 2 && flags & FLAG_HAS_ROUTE != 0;

    let payload_length: usize = if version >= 2 {
        read_u32_be(raw, &mut pos)? as usize
    } else {
        read_u16_be(raw, &mut pos)? as usize
    };

    if payload_length > MAX_PAYLOAD_LENGTH {
        return None;
    }

    // Minimum size check before reading variable sections. The route hop count
    // is peeked from its absolute position (after sender and recipient).
    let mut expected_size = header_size(version) + SENDER_ID_SIZE + payload_length;
    if has_recipient {
        expected_size += RECIPIENT_ID_SIZE;
    }
    let mut route_count = 0usize;
    if has_route {
        let route_offset = header_size(version)
            + SENDER_ID_SIZE
            + if has_recipient { RECIPIENT_ID_SIZE } else { 0 };
        if raw.len() > route_offset {
            route_count = raw[route_offset] as usize;
        }
        expected_size += 1 + route_count * SENDER_ID_SIZE;
    }
    if has_signature {
        expected_size += SIGNATURE_SIZE;
    }
    if raw.len() < expected_size {
        return None;
    }

    let mut sender_id = [0u8; SENDER_ID_SIZE];
    sender_id.copy_from_slice(&raw[pos..pos + SENDER_ID_SIZE]);
    pos += SENDER_ID_SIZE;

    let recipient_id = if has_recipient {
        let mut id = [0u8; RECIPIENT_ID_SIZE];
        id.copy_from_slice(&raw[pos..pos + RECIPIENT_ID_SIZE]);
        pos += RECIPIENT_ID_SIZE;
        Some(id)
    } else {
        None
    };

    let route: Option<Vec<[u8; SENDER_ID_SIZE]>> = if has_route {
        let count = raw[pos] as usize;
        pos += 1;
        if count == 0 {
            // Canonical representation: an empty route list decodes as absent.
            None
        } else {
            let mut hops = Vec::with_capacity(count);
            for _ in 0..count {
                let mut hop = [0u8; SENDER_ID_SIZE];
                hop.copy_from_slice(&raw[pos..pos + SENDER_ID_SIZE]);
                pos += SENDER_ID_SIZE;
                hops.push(hop);
            }
            Some(hops)
        }
    } else {
        None
    };

    let (payload, wire_payload): (Vec<u8>, Option<WirePayload>) = if is_compressed {
        let length_field_bytes = if version >= 2 { 4 } else { 2 };
        if payload_length < length_field_bytes {
            return None;
        }
        let original_size = if version >= 2 {
            read_u32_be(raw, &mut pos)? as usize
        } else {
            read_u16_be(raw, &mut pos)? as usize
        };

        if original_size == 0 || original_size > MAX_PAYLOAD_LENGTH {
            return None;
        }

        let compressed_size = payload_length - length_field_bytes;
        if compressed_size == 0 {
            return None;
        }
        let ratio = original_size as f64 / compressed_size as f64;
        if ratio > MAX_COMPRESSION_RATIO {
            return None;
        }

        let compressed = raw[pos..pos + compressed_size].to_vec();
        pos += compressed_size;
        let expanded = decompress(&compressed, original_size)?;
        if expanded.len() != original_size {
            return None;
        }
        let wire = WirePayload {
            bytes: Rc::from(compressed.as_slice()),
            compressed: true,
            for_payload: Rc::from(expanded.as_slice()),
        };
        (expanded, Some(wire))
    } else {
        let bytes = raw[pos..pos + payload_length].to_vec();
        pos += payload_length;
        let wire = WirePayload {
            bytes: Rc::from(bytes.as_slice()),
            compressed: false,
            for_payload: Rc::from(bytes.as_slice()),
        };
        (bytes, Some(wire))
    };

    let signature = if has_signature {
        let mut sig = [0u8; SIGNATURE_SIZE];
        sig.copy_from_slice(&raw[pos..pos + SIGNATURE_SIZE]);
        Some(sig)
    } else {
        None
    };

    Some(Packet {
        version,
        packet_type,
        sender_id,
        recipient_id,
        timestamp,
        payload,
        signature,
        ttl,
        route,
        wire_payload,
    })
}
