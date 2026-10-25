//! BitChat desktop node: a headless mesh participant.
//!
//! Layers:
//! - [`identity`]: persistent Noise + Ed25519 identity with the wire peer ID
//!   derived exactly like the Android client (`SHA-256(Noise static pubkey)`).
//! - [`link`]: session orchestration on top of the BLE central — announce,
//!   Noise XX handshake (initiator and responder), encrypted private chat.

pub mod identity;
pub mod link;

pub use identity::NodeIdentity;
pub use link::{LinkCommand, LinkEvent, MeshLink};

/// Errors surfaced by the node layer.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("BLE central: {0}")]
    Ble(#[from] bitchat_ble::central::CentralError),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("identity: {0}")]
    Identity(String),
    #[error("link closed")]
    Closed,
}
