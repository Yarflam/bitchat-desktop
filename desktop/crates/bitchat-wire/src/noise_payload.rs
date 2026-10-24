//! Noise payload envelope and private message TLV encoding.
//! Byte-exact port of `NoiseEncrypted.kt` (iOS-compatible).

/// Payload types embedded within NOISE_ENCRYPTED messages.
/// Matches iOS `NoisePayloadType` exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NoisePayloadType {
    PrivateMessage = 0x01,
    ReadReceipt = 0x02,
    Delivered = 0x03,
    VoiceFrame = 0x08,
    VerifyChallenge = 0x10,
    VerifyResponse = 0x11,
    FileTransfer = 0x20,
    /// Authenticated capabilities + Ed25519 binding for the current Noise generation.
    PeerState = 0x21,
}

/// Prerelease iOS builds briefly emitted private files as 0x09. Decode-only
/// alias: every payload this crate constructs still encodes the canonical
/// 0x20 value, so the alias cannot leak into new traffic.
const PRERELEASE_FILE_TRANSFER_RAW_VALUE: u8 = 0x09;

impl NoisePayloadType {
    pub fn from_value(value: u8) -> Option<Self> {
        Some(match value {
            0x01 => Self::PrivateMessage,
            0x02 => Self::ReadReceipt,
            0x03 => Self::Delivered,
            0x08 => Self::VoiceFrame,
            0x10 => Self::VerifyChallenge,
            0x11 => Self::VerifyResponse,
            0x20 | PRERELEASE_FILE_TRANSFER_RAW_VALUE => Self::FileTransfer,
            0x21 => Self::PeerState,
            _ => return None,
        })
    }
}

/// `[type:1][payload:n]` envelope inside NOISE_ENCRYPTED packets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoisePayload {
    pub payload_type: NoisePayloadType,
    pub data: Vec<u8>,
}

impl NoisePayload {
    pub fn new(payload_type: NoisePayloadType, data: Vec<u8>) -> Self {
        Self { payload_type, data }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + self.data.len());
        out.push(self.payload_type as u8);
        out.extend_from_slice(&self.data);
        out
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        let (&first, rest) = data.split_first()?;
        let payload_type = NoisePayloadType::from_value(first)?;
        Some(Self {
            payload_type,
            data: rest.to_vec(),
        })
    }
}

/// Private message packet with TLV encoding, matching iOS
/// `PrivateMessagePacket` exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateMessagePacket {
    pub message_id: String,
    pub content: String,
}

const TLV_MESSAGE_ID: u8 = 0x00;
const TLV_CONTENT: u8 = 0x01;

impl PrivateMessagePacket {
    pub fn new(message_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            message_id: message_id.into(),
            content: content.into(),
        }
    }

    /// `[type][length][value]` per field; `None` when a field exceeds the
    /// 1-byte length limit.
    pub fn encode(&self) -> Option<Vec<u8>> {
        let message_id = self.message_id.as_bytes();
        let content = self.content.as_bytes();
        if message_id.len() > 255 || content.len() > 255 {
            return None;
        }
        let mut out = Vec::with_capacity(2 + message_id.len() + 2 + content.len());
        out.push(TLV_MESSAGE_ID);
        out.push(message_id.len() as u8);
        out.extend_from_slice(message_id);
        out.push(TLV_CONTENT);
        out.push(content.len() as u8);
        out.extend_from_slice(content);
        Some(out)
    }

    /// Unknown TLV types are rejected (unlike announcements/files), matching
    /// the Kotlin decoder's `?: return null`.
    pub fn decode(data: &[u8]) -> Option<Self> {
        let mut offset = 0usize;
        let mut message_id = None;
        let mut content = None;

        while offset + 2 <= data.len() {
            let tlv_type = data[offset];
            if tlv_type != TLV_MESSAGE_ID && tlv_type != TLV_CONTENT {
                return None;
            }
            offset += 1;
            let length = data[offset] as usize;
            offset += 1;
            if offset + length > data.len() {
                return None;
            }
            let value = String::from_utf8_lossy(&data[offset..offset + length]).into_owned();
            offset += length;
            match tlv_type {
                TLV_MESSAGE_ID => message_id = Some(value),
                _ => content = Some(value),
            }
        }

        Some(Self {
            message_id: message_id?,
            content: content?,
        })
    }
}
