//! Persistent node identity: Noise static keypair (wire peer ID source) plus
//! Ed25519 signing key, matching how the Android client derives `myPeerID`
//! (`SHA-256(Noise static public key)`, first 16 hex chars).

use std::path::Path;

use bitchat_wire::identity::Identity;
use bitchat_wire::noise::{derive_peer_id, PROTOCOL};

use crate::LinkError;

/// A node's long-term identity. All fields are persisted together so the
/// wire peer ID stays stable across restarts.
pub struct NodeIdentity {
    pub noise_private_key: [u8; 32],
    pub noise_public_key: [u8; 32],
    pub signing: Identity,
    /// 16 lowercase hex chars, as used in packet sender/recipient fields.
    pub peer_id: String,
}

impl NodeIdentity {
    pub fn from_parts(
        noise_private_key: [u8; 32],
        noise_public_key: [u8; 32],
        signing: Identity,
    ) -> Self {
        let peer_id = derive_peer_id(&noise_public_key);
        Self {
            noise_private_key,
            noise_public_key,
            signing,
            peer_id,
        }
    }

    pub fn generate() -> Result<Self, LinkError> {
        let params = PROTOCOL
            .parse()
            .map_err(|e| LinkError::Identity(format!("noise params: {e}")))?;
        let keypair = snow::Builder::new(params)
            .generate_keypair()
            .map_err(|e| LinkError::Identity(format!("noise keypair generation failed: {e}")))?;
        let mut noise_private_key = [0u8; 32];
        let mut noise_public_key = [0u8; 32];
        noise_private_key.copy_from_slice(&keypair.private[..32]);
        noise_public_key.copy_from_slice(&keypair.public[..32]);
        Ok(Self::from_parts(
            noise_private_key,
            noise_public_key,
            Identity::generate(),
        ))
    }

    /// Load from `path`, or generate and persist a fresh identity.
    pub fn load_or_generate(path: &Path) -> Result<Self, LinkError> {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Some(identity) = Self::parse(&text) {
                return Ok(identity);
            }
            return Err(LinkError::Identity(format!(
                "identity file {} is corrupt; refusing to overwrite it",
                path.display()
            )));
        }
        let identity = Self::generate()?;
        identity.save(path)?;
        Ok(identity)
    }

    fn parse(text: &str) -> Option<Self> {
        let mut noise_private = None;
        let mut noise_public = None;
        let mut signing = None;
        for line in text.lines() {
            let (key, value) = line.split_once('=')?;
            match key {
                "noise_private" => noise_private = decode_hex_array::<32>(value),
                "noise_public" => noise_public = decode_hex_array::<32>(value),
                "signing" => {
                    signing = decode_hex_array::<32>(value).map(|s| Identity::from_secret_key(&s))
                }
                _ => {}
            }
        }
        Some(Self::from_parts(noise_private?, noise_public?, signing?))
    }

    pub fn save(&self, path: &Path) -> Result<(), LinkError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(LinkError::Io)?;
        }
        let text = format!(
            "noise_private={}\nnoise_public={}\nsigning={}\n",
            encode_hex(&self.noise_private_key),
            encode_hex(&self.noise_public_key),
            encode_hex(&self.signing.secret_key()),
        );
        std::fs::write(path, text).map_err(LinkError::Io)
    }

    /// Wire form of the peer ID (8 raw bytes used in packet fields).
    pub fn peer_id_bytes(&self) -> [u8; 8] {
        decode_hex_array::<8>(&self.peer_id).unwrap_or([0u8; 8])
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn decode_hex_array<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, chunk) in text.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        out[i] = ((hi << 4) | lo) as u8;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_id_matches_noise_key_derivation() {
        let identity = NodeIdentity::generate().expect("generate");
        assert_eq!(identity.peer_id.len(), 16);
        assert_eq!(identity.peer_id, derive_peer_id(&identity.noise_public_key));
        assert_eq!(identity.peer_id_bytes().len(), 8);
    }

    #[test]
    fn persistence_roundtrip_keeps_identity() {
        let dir = std::env::temp_dir().join(format!("bitchat-node-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("identity.txt");

        let first = NodeIdentity::load_or_generate(&path).expect("first");
        let second = NodeIdentity::load_or_generate(&path).expect("second");
        assert_eq!(first.peer_id, second.peer_id);
        assert_eq!(first.noise_public_key, second.noise_public_key);
        assert_eq!(first.signing.public_key(), second.signing.public_key());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
