//! Ed25519 identity and packet signatures.
//!
//! Mirror of the deployed signing rules: the signature input is the packet
//! re-encoded with the signature omitted and TTL normalized to 0
//! (`Packet::encode_for_signing`, port of `toBinaryDataForSigning()`), so
//! relays may decrement TTL without invalidating signatures. Verification
//! uses the peer's Ed25519 public key from their signed ANNOUNCE (TLV 0x03).

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

use crate::packet::Packet;

pub const SIGNATURE_SIZE: usize = 64;
pub const PUBLIC_KEY_SIZE: usize = 32;
pub const SECRET_KEY_SIZE: usize = 32;

#[derive(Debug, Clone)]
pub struct Identity {
    signing: SigningKey,
}

impl Identity {
    pub fn from_secret_key(secret: &[u8; SECRET_KEY_SIZE]) -> Self {
        Self {
            signing: SigningKey::from_bytes(secret),
        }
    }

    pub fn generate() -> Self {
        let mut rng = rand_core::OsRng;
        Self {
            signing: SigningKey::generate(&mut rng),
        }
    }

    pub fn secret_key(&self) -> [u8; SECRET_KEY_SIZE] {
        self.signing.to_bytes()
    }

    pub fn public_key(&self) -> [u8; PUBLIC_KEY_SIZE] {
        self.signing.verifying_key().to_bytes()
    }

    /// Sign a packet's canonical preimage (signature omitted, TTL zeroed).
    pub fn sign_packet(&self, packet: &Packet) -> Option<[u8; SIGNATURE_SIZE]> {
        let preimage = packet.encode_for_signing()?;
        let signature: Signature = self.signing.sign(&preimage);
        Some(signature.to_bytes())
    }

    pub fn sign(&self, message: &[u8]) -> [u8; SIGNATURE_SIZE] {
        self.signing.sign(message).to_bytes()
    }
}

/// Verify a detached Ed25519 signature, mirroring
/// `EncryptionService.verifyEd25519Signature` (any error is `false`).
pub fn verify(
    public_key: &[u8; PUBLIC_KEY_SIZE],
    message: &[u8],
    signature: &[u8; SIGNATURE_SIZE],
) -> bool {
    let Ok(verifying) = VerifyingKey::from_bytes(public_key) else {
        return false;
    };
    let signature = Signature::from_bytes(signature);
    verifying.verify(message, &signature).is_ok()
}

/// Verify the packet's embedded signature against a peer's public key.
/// Returns `false` for unsigned packets, mirroring the mandatory-signature
/// check in `SecurityManager.verifyPacketSignature`.
pub fn verify_packet(packet: &Packet, public_key: &[u8; PUBLIC_KEY_SIZE]) -> bool {
    let Some(signature) = &packet.signature else {
        return false;
    };
    let Some(preimage) = packet.encode_for_signing() else {
        return false;
    };
    verify(public_key, &preimage, signature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MessageType, Packet};

    fn hex(value: &str) -> Vec<u8> {
        value
            .as_bytes()
            .chunks(2)
            .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
            .collect()
    }

    #[test]
    fn fixed_ed25519_vector_from_reference_implementation() {
        // Synthetic fixture (seed 0x00..0x1f, NOT a real identity) generated
        // with a reference Ed25519 implementation to pin primitive
        // compatibility across crypto backends.
        let secret: [u8; 32] =
            hex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f")
                .try_into()
                .unwrap();
        let expected_public =
            hex("03a107bff3ce10be1d70dd18e74bc09967e4d6309ba50d5f1ddc8664125531b8");
        let message = hex("626974636861742d656432353531392d766563746f72");
        let expected_signature = hex(
            "2c1e8940cc6594c7ce18e198f79c3661f724e0b237fc797fb96b893523d23d7c\
             dd00093f8d09d7d37115c4b50097db30f01fc0e24f32e9bd29e44de9c793cc02",
        );

        let identity = Identity::from_secret_key(&secret);
        assert_eq!(expected_public, identity.public_key().to_vec());
        assert_eq!(expected_signature, identity.sign(&message).to_vec());
        let signature: [u8; SIGNATURE_SIZE] = expected_signature.try_into().unwrap();
        assert!(verify(&identity.public_key(), &message, &signature));
    }

    #[test]
    fn sign_verify_round_trip_and_tamper_rejection() {
        let identity = Identity::generate();
        let mut packet = Packet::new(
            MessageType::Announce as u8,
            7,
            [0x11; 8],
            b"payload".to_vec(),
        );
        packet.timestamp = 0x0102030405060708;

        let signature = identity.sign_packet(&packet).unwrap();
        packet.signature = Some(signature);

        assert!(verify_packet(&packet, &identity.public_key()));

        // Payload tampering invalidates the signature.
        let mut tampered = packet.clone();
        tampered.payload = b"evil!!!".to_vec();
        assert!(!verify_packet(&tampered, &identity.public_key()));

        // TTL changes are relay-transparent: the preimage normalizes TTL to 0,
        // so a decremented TTL must still verify.
        let mut relayed = packet.clone();
        relayed.ttl = 3;
        assert!(verify_packet(&relayed, &identity.public_key()));

        // Unsigned packets are rejected.
        let unsigned = packet.clone();
        let mut unsigned = unsigned;
        unsigned.signature = None;
        assert!(!verify_packet(&unsigned, &identity.public_key()));

        // Wrong key rejects.
        let other = Identity::generate();
        assert!(!verify_packet(&packet, &other.public_key()));
    }
}
