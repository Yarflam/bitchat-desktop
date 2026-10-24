//! Noise XX session, port of `NoiseSession.kt` + `NoisePeerIdentity.kt`.
//!
//! Handshake: `Noise_XX_25519_ChaChaPoly_SHA256`, no prologue (the Cacophony
//! vector in `tests/noise_vector.rs` pins transcript bytes with a prologue).
//! Transport frames are `<nonce:4 BE><ciphertext>`, nonces are explicit, and
//! a 1024-bit sliding window rejects replays — identical to Android/iOS.

use sha2::{Digest, Sha256};
use snow::TransportState;

/// Mirror of `AppConstants.Noise`.
pub const REKEY_TIME_LIMIT_MS: u64 = 3_600_000;
pub const REKEY_MESSAGE_LIMIT_SESSION: u64 = 10_000;
pub const MAX_PAYLOAD_SIZE_BYTES: usize = 256;
pub const STATIC_PUBLIC_KEY_SIZE: usize = 32;

/// XX pattern message sizes on the wire. Handshake payloads are always
/// encrypted, so messages 2 and 3 carry the 16-byte MAC of the (empty)
/// payload; this matches both snow and the deployed noise-java writer, whose
/// null-payload branch still calls encryptAndHash(..., 0).
pub const XX_MESSAGE_1_SIZE: usize = 32;
pub const XX_MESSAGE_2_SIZE: usize = 96;
pub const XX_MESSAGE_3_SIZE: usize = 64;

const NONCE_SIZE_BYTES: usize = 4;
const REPLAY_WINDOW_SIZE: u64 = 1024;
const REPLAY_WINDOW_BYTES: usize = (REPLAY_WINDOW_SIZE / 8) as usize;

const PROTOCOL: &str = "Noise_XX_25519_ChaChaPoly_SHA256";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoiseError {
    InvalidState(String),
    HandshakeFailed,
    PeerIdentityMismatch { claimed: String, derived: String },
    EncryptionFailed,
    DecryptionFailed,
    NonceExceeded,
}

impl std::fmt::Display for NoiseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidState(s) => write!(f, "invalid state: {s}"),
            Self::HandshakeFailed => write!(f, "handshake failed"),
            Self::PeerIdentityMismatch { claimed, derived } => {
                write!(
                    f,
                    "peer identity mismatch: claimed {claimed}, derived {derived}"
                )
            }
            Self::EncryptionFailed => write!(f, "encryption failed"),
            Self::DecryptionFailed => write!(f, "decryption failed"),
            Self::NonceExceeded => write!(f, "nonce exceeds 4-byte limit"),
        }
    }
}

impl std::error::Error for NoiseError {}

/// Mesh wire identity: first eight bytes of SHA-256(staticPublicKey) as 16
/// lowercase hex chars. Mirror of `NoisePeerIdentity.kt`.
pub fn derive_peer_id(static_public_key: &[u8; 32]) -> String {
    let digest = Sha256::digest(static_public_key);
    to_hex(&digest[..8])
}

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Uninitialized,
    Handshaking,
    Established,
    Failed,
}

/// One Noise session with a specific peer, mirroring `NoiseSession.kt`.
pub struct NoiseSession {
    peer_id: String,
    is_initiator: bool,
    local_static_private_key: [u8; 32],
    state: SessionState,
    handshake: Option<snow::HandshakeState>,
    transport: Option<TransportState>,
    creation_ms: u64,
    messages_sent: u64,
    messages_received: u64,
    highest_received_nonce: u64,
    replay_window: [u8; REPLAY_WINDOW_BYTES],
    remote_static_public_key: Option<[u8; 32]>,
    handshake_hash: Option<[u8; 32]>,
}

impl NoiseSession {
    /// Validates static keys like the Kotlin constructor (32 bytes, non-zero).
    pub fn new(
        peer_id: &str,
        is_initiator: bool,
        local_static_private_key: [u8; 32],
        creation_ms: u64,
    ) -> Result<Self, NoiseError> {
        if local_static_private_key.iter().all(|&b| b == 0) {
            return Err(NoiseError::InvalidState(
                "local static private key cannot be all zeros".to_string(),
            ));
        }
        Ok(Self {
            peer_id: peer_id.to_string(),
            is_initiator,
            local_static_private_key,
            state: SessionState::Uninitialized,
            handshake: None,
            transport: None,
            creation_ms,
            messages_sent: 0,
            messages_received: 0,
            highest_received_nonce: 0,
            replay_window: [0u8; REPLAY_WINDOW_BYTES],
            remote_static_public_key: None,
            handshake_hash: None,
        })
    }

    pub fn state(&self) -> SessionState {
        self.state
    }

    pub fn is_established(&self) -> bool {
        self.state == SessionState::Established
    }

    pub fn remote_static_public_key(&self) -> Option<[u8; 32]> {
        self.remote_static_public_key
    }

    pub fn handshake_hash(&self) -> Option<[u8; 32]> {
        self.handshake_hash
    }

    fn build_handshake(&self) -> Result<snow::HandshakeState, NoiseError> {
        let builder =
            snow::Builder::new(PROTOCOL.parse().map_err(|_| NoiseError::HandshakeFailed)?)
                .local_private_key(&self.local_static_private_key)
                .map_err(|_| NoiseError::HandshakeFailed)?;
        if self.is_initiator {
            builder.build_initiator()
        } else {
            builder.build_responder()
        }
        .map_err(|_| NoiseError::HandshakeFailed)
    }

    /// Initiator only: returns message 1 (`e`, 32 bytes).
    pub fn start_handshake(&mut self) -> Result<Vec<u8>, NoiseError> {
        if !self.is_initiator {
            return Err(NoiseError::InvalidState(
                "only initiator can start handshake".to_string(),
            ));
        }
        if self.state != SessionState::Uninitialized {
            return Err(NoiseError::InvalidState(
                "handshake already started".to_string(),
            ));
        }
        let mut hs = self.build_handshake()?;
        let mut message = vec![0u8; 512];
        let len = hs.write_message(&[], &mut message).map_err(|e| {
            self.state = SessionState::Failed;
            NoiseError::InvalidState(format!("write message 1 failed: {e}"))
        })?;
        message.truncate(len);
        self.handshake = Some(hs);
        self.state = SessionState::Handshaking;
        Ok(message)
    }

    /// Process one handshake message. Returns the response bytes when the
    /// pattern expects one (message 2 or 3), or `None` on SPLIT.
    pub fn process_handshake_message(
        &mut self,
        message: &[u8],
    ) -> Result<Option<Vec<u8>>, NoiseError> {
        if self.state == SessionState::Uninitialized && !self.is_initiator {
            self.handshake = Some(self.build_handshake()?);
            self.state = SessionState::Handshaking;
        }
        if self.state != SessionState::Handshaking {
            self.state = SessionState::Failed;
            return Err(NoiseError::InvalidState(format!(
                "invalid state for handshake: {:?}",
                self.state
            )));
        }

        let mut hs = self.handshake.take().ok_or_else(|| {
            self.state = SessionState::Failed;
            NoiseError::HandshakeFailed
        })?;

        let mut payload_buf = vec![0u8; XX_MESSAGE_2_SIZE + MAX_PAYLOAD_SIZE_BYTES];
        if hs.read_message(message, &mut payload_buf).is_err() {
            self.state = SessionState::Failed;
            return Err(NoiseError::HandshakeFailed);
        }

        if hs.is_handshake_finished() {
            // SPLIT: message 3 consumed; complete and answer nothing.
            self.complete_handshake(hs)?;
            return Ok(None);
        }

        if hs.is_my_turn() {
            let mut response = vec![0u8; 512];
            let len = hs.write_message(&[], &mut response).map_err(|_| {
                self.state = SessionState::Failed;
                NoiseError::HandshakeFailed
            })?;
            response.truncate(len);
            // After writing message 3 the initiator side is finished.
            if hs.is_handshake_finished() {
                self.complete_handshake(hs)?;
            } else {
                self.handshake = Some(hs);
            }
            return Ok(Some(response));
        }

        self.handshake = Some(hs);
        Ok(None)
    }

    /// Authenticate the remote static key against the claimed wire identity
    /// BEFORE any transport cipher exists, then split. Mirror of
    /// `NoiseSession.completeHandshake`.
    fn complete_handshake(&mut self, hs: snow::HandshakeState) -> Result<(), NoiseError> {
        let remote = hs.get_remote_static().ok_or_else(|| {
            self.state = SessionState::Failed;
            NoiseError::HandshakeFailed
        })?;
        let remote: [u8; 32] = remote.try_into().map_err(|_| {
            self.state = SessionState::Failed;
            NoiseError::HandshakeFailed
        })?;
        let derived = derive_peer_id(&remote);
        if derived != self.peer_id {
            self.state = SessionState::Failed;
            return Err(NoiseError::PeerIdentityMismatch {
                claimed: self.peer_id.clone(),
                derived,
            });
        }
        // Copy the handshake hash before consuming the state.
        let hash: [u8; 32] = hs.get_handshake_hash().try_into().map_err(|_| {
            self.state = SessionState::Failed;
            NoiseError::HandshakeFailed
        })?;

        let transport = hs.into_transport_mode().map_err(|_| {
            self.state = SessionState::Failed;
            NoiseError::HandshakeFailed
        })?;

        self.remote_static_public_key = Some(remote);
        self.handshake_hash = Some(hash);
        self.transport = Some(transport);
        self.messages_sent = 0;
        self.messages_received = 0;
        self.highest_received_nonce = 0;
        self.replay_window = [0u8; REPLAY_WINDOW_BYTES];
        self.handshake = None;
        self.state = SessionState::Established;
        Ok(())
    }

    /// Encrypt as `<nonce:4 BE><ciphertext>`, nonce = messagesSent.
    pub fn encrypt(&mut self, data: &[u8]) -> Result<Vec<u8>, NoiseError> {
        if !self.is_established() {
            return Err(NoiseError::InvalidState(
                "session not established".to_string(),
            ));
        }
        if self.messages_sent > u32::MAX as u64 - 1 {
            return Err(NoiseError::NonceExceeded);
        }
        let transport = self
            .transport
            .as_mut()
            .ok_or(NoiseError::EncryptionFailed)?;
        let mut ciphertext = vec![0u8; data.len() + 16];
        transport
            .write_message(data, &mut ciphertext)
            .map_err(|_| NoiseError::EncryptionFailed)?;
        let current_nonce = self.messages_sent;
        self.messages_sent += 1;

        let mut combined = Vec::with_capacity(NONCE_SIZE_BYTES + ciphertext.len());
        combined.extend_from_slice(&(current_nonce as u32).to_be_bytes());
        combined.extend_from_slice(&ciphertext);
        Ok(combined)
    }

    /// Decrypt a `<nonce:4 BE><ciphertext>` frame with sliding-window replay
    /// protection. Mirrors `NoiseSession.decrypt` fail-closed behavior.
    pub fn decrypt(&mut self, combined: &[u8]) -> Result<Vec<u8>, NoiseError> {
        if !self.is_established() {
            return Err(NoiseError::InvalidState(
                "session not established".to_string(),
            ));
        }
        if combined.len() < NONCE_SIZE_BYTES + 16 {
            return Err(NoiseError::DecryptionFailed);
        }
        let nonce = u32::from_be_bytes([combined[0], combined[1], combined[2], combined[3]]) as u64;
        let ciphertext = &combined[NONCE_SIZE_BYTES..];

        if !is_valid_nonce(nonce, self.highest_received_nonce, &self.replay_window) {
            return Err(NoiseError::DecryptionFailed);
        }

        let transport = self
            .transport
            .as_mut()
            .ok_or(NoiseError::DecryptionFailed)?;
        transport.set_receiving_nonce(nonce);
        let mut plaintext = vec![0u8; ciphertext.len()];
        let len = transport
            .read_message(ciphertext, &mut plaintext)
            .map_err(|_| NoiseError::DecryptionFailed)?;
        plaintext.truncate(len);

        mark_nonce_as_seen(
            nonce,
            &mut self.highest_received_nonce,
            &mut self.replay_window,
        );
        self.messages_received += 1;
        Ok(plaintext)
    }

    pub fn needs_rekey(&self, now_ms: u64) -> bool {
        if !self.is_established() {
            return false;
        }
        now_ms.saturating_sub(self.creation_ms) > REKEY_TIME_LIMIT_MS
            || (self.messages_sent + self.messages_received) > REKEY_MESSAGE_LIMIT_SESSION
    }

    pub fn messages_sent(&self) -> u64 {
        self.messages_sent
    }

    pub fn messages_received(&self) -> u64 {
        self.messages_received
    }
}

/// Sliding-window replay check, bit-exact port of `NoiseSession.isValidNonce`.
fn is_valid_nonce(received: u64, highest: u64, window: &[u8; REPLAY_WINDOW_BYTES]) -> bool {
    if received + REPLAY_WINDOW_SIZE <= highest {
        return false; // too old, outside window
    }
    if received > highest {
        return true; // always accept newer nonces
    }
    let offset = (highest - received) as usize;
    let byte_index = offset / 8;
    let bit_index = offset % 8;
    window[byte_index] & (1 << bit_index) == 0
}

/// Mark a nonce seen, bit-exact port of `NoiseSession.markNonceAsSeen`.
fn mark_nonce_as_seen(received: u64, highest: &mut u64, window: &mut [u8; REPLAY_WINDOW_BYTES]) {
    if received > *highest {
        let shift = (received - *highest) as usize;
        if shift >= REPLAY_WINDOW_SIZE as usize {
            window.fill(0);
        } else {
            for i in (0..REPLAY_WINDOW_BYTES).rev() {
                let source_byte_index = i as isize - (shift / 8) as isize;
                let mut new_byte = 0u8;
                if source_byte_index >= 0 {
                    new_byte = window[source_byte_index as usize] >> (shift % 8);
                    if source_byte_index > 0 && shift % 8 != 0 {
                        new_byte |= window[source_byte_index as usize - 1] << (8 - shift % 8);
                    }
                }
                window[i] = new_byte;
            }
        }
        *highest = received;
        window[0] |= 1; // mark most recent bit as seen
    } else {
        let offset = (*highest - received) as usize;
        window[offset / 8] |= 1 << (offset % 8);
    }
}
