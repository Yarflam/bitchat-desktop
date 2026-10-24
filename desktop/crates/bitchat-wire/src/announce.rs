//! Identity announcement (ANNOUNCE payload) and capability bitfield.
//! Byte-exact port of `IdentityAnnouncement.kt` and `PeerCapabilities.kt`.

/// Feature bits advertised in TLV `0x05`; little-endian minimal bitfield.
/// Bit assignment matches iOS and `docs/ANNOUNCEMENT_GOSSIP.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCapabilities(pub u64);

impl PeerCapabilities {
    pub const NONE: Self = Self(0);
    pub const PREKEYS: Self = Self(1 << 0);
    pub const WIFI_BULK: Self = Self(1 << 1);
    pub const GATEWAY: Self = Self(1 << 2);
    pub const GROUPS: Self = Self(1 << 3);
    pub const BOARD: Self = Self(1 << 4);
    pub const VOUCH: Self = Self(1 << 5);
    pub const MESH_DIAGNOSTICS: Self = Self(1 << 6);
    pub const BRIDGE: Self = Self(1 << 7);
    /// Noise-encrypted private BitchatFilePacket using payload type 0x20.
    pub const PRIVATE_MEDIA: Self = Self(1 << 8);
    pub const PRIVATE_MEDIA_RECEIPTS: Self = Self(1 << 9);
    /// Reserved by iOS; decode but do not advertise or act on it.
    pub const NON_DESTRUCTIVE_NOISE_REPLACEMENT: Self = Self(1 << 10);

    pub fn contains(&self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Minimal little-endian encoding: always at least one byte.
    pub fn encode(&self) -> Vec<u8> {
        let mut remaining = self.0;
        let mut bytes = Vec::new();
        loop {
            bytes.push(remaining as u8);
            remaining >>= 8;
            if remaining == 0 {
                break;
            }
        }
        bytes
    }

    /// Decode the low 64 bits, ignoring any future extension bytes.
    pub fn decode(data: &[u8]) -> Self {
        let mut raw = 0u64;
        for (index, &byte) in data.iter().take(8).enumerate() {
            raw |= (byte as u64) << (8 * index);
        }
        Self(raw)
    }
}

/// An announcement TLV not understood by this build, retained verbatim so a
/// decode/re-encode cycle does not erase extensions added by newer clients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownTlv {
    pub tlv_type: u8,
    pub value: Vec<u8>,
}

const TLV_NICKNAME: u8 = 0x01;
const TLV_NOISE_PUBLIC_KEY: u8 = 0x02;
const TLV_SIGNING_PUBLIC_KEY: u8 = 0x03;
const TLV_DIRECT_NEIGHBORS: u8 = 0x04;
const TLV_CAPABILITIES: u8 = 0x05;

/// Identity announcement with TLV encoding, matching the iOS/Android format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityAnnouncement {
    pub nickname: String,
    pub noise_public_key: Vec<u8>,
    pub signing_public_key: Vec<u8>,
    pub capabilities: Option<PeerCapabilities>,
    /// Extensions this build does not understand, retained in order. This
    /// includes gossip TLV `0x04` when decoded through this model.
    pub unknown_tlvs: Vec<UnknownTlv>,
}

impl IdentityAnnouncement {
    /// Encode to TLV binary data. Returns `None` when any field exceeds the
    /// 1-byte TLV length limit.
    pub fn encode(&self) -> Option<Vec<u8>> {
        let nickname = self.nickname.as_bytes();
        if nickname.len() > 255
            || self.noise_public_key.len() > 255
            || self.signing_public_key.len() > 255
            || self.unknown_tlvs.iter().any(|t| t.value.len() > 255)
        {
            return None;
        }

        let mut out = Vec::new();
        put_tlv(&mut out, TLV_NICKNAME, nickname);
        put_tlv(&mut out, TLV_NOISE_PUBLIC_KEY, &self.noise_public_key);
        put_tlv(&mut out, TLV_SIGNING_PUBLIC_KEY, &self.signing_public_key);
        if let Some(caps) = &self.capabilities {
            put_tlv(&mut out, TLV_CAPABILITIES, &caps.encode());
        }
        for tlv in &self.unknown_tlvs {
            put_tlv(&mut out, tlv.tlv_type, &tlv.value);
        }
        Some(out)
    }

    /// Decode TLVs in order; unknown TLVs are skipped and retained.
    /// All three identity fields are required, else `None` (fail-closed).
    pub fn decode(data: &[u8]) -> Option<Self> {
        let mut offset = 0usize;
        let mut nickname = None;
        let mut noise_public_key = None;
        let mut signing_public_key = None;
        let mut capabilities = None;
        let mut unknown_tlvs = Vec::new();

        while offset + 2 <= data.len() {
            let tlv_type = data[offset];
            offset += 1;
            let length = data[offset] as usize;
            offset += 1;
            if offset + length > data.len() {
                return None;
            }
            let value = &data[offset..offset + length];
            offset += length;

            match tlv_type {
                TLV_NICKNAME => {
                    nickname = Some(String::from_utf8_lossy(value).into_owned());
                }
                TLV_NOISE_PUBLIC_KEY => noise_public_key = Some(value.to_vec()),
                TLV_SIGNING_PUBLIC_KEY => signing_public_key = Some(value.to_vec()),
                TLV_CAPABILITIES => capabilities = Some(PeerCapabilities::decode(value)),
                _ => unknown_tlvs.push(UnknownTlv {
                    tlv_type,
                    value: value.to_vec(),
                }),
            }
        }

        Some(Self {
            nickname: nickname?,
            noise_public_key: noise_public_key?,
            signing_public_key: signing_public_key?,
            capabilities,
            unknown_tlvs,
        })
    }

    /// Direct neighbors from gossip TLV `0x04`, when present: up to 10 peer
    /// IDs of 8 bytes each. A non-multiple-of-8 length leaves trailing partial
    /// bytes ignored.
    pub fn direct_neighbors(&self) -> Vec<[u8; 8]> {
        for tlv in &self.unknown_tlvs {
            if tlv.tlv_type != TLV_DIRECT_NEIGHBORS {
                continue;
            }
            return tlv
                .value
                .chunks_exact(8)
                .map(|c| c.try_into().expect("chunks_exact(8)"))
                .collect();
        }
        Vec::new()
    }
}

fn put_tlv(out: &mut Vec<u8>, tlv_type: u8, value: &[u8]) {
    out.push(tlv_type);
    out.push(value.len() as u8);
    out.extend_from_slice(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_minimal_little_endian_encoding() {
        assert_eq!(vec![0x00], PeerCapabilities::NONE.encode());
        assert_eq!(vec![0x01], PeerCapabilities::PREKEYS.encode());
        // Bit 8's exact value is 00 01 (little-endian 0x100).
        assert_eq!(vec![0x00, 0x01], PeerCapabilities::PRIVATE_MEDIA.encode());
        // Decode keeps unknown bits in the low 64 bits and ignores bytes beyond.
        let decoded =
            PeerCapabilities::decode(&[0x00, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xAA]);
        assert!(decoded.contains(PeerCapabilities::PRIVATE_MEDIA));
        assert_eq!(PeerCapabilities(0xFFFF_FFFF_FFFF_0100), decoded);
    }
}
