//! BitChat wire protocol codec.
//!
//! Byte-exact Rust port of the Android client's framing
//! (`app/src/main/java/com/bitchat/android/protocol/BinaryProtocol.kt`).
//! Compatibility is pinned by literal-byte tests in `tests/wire_contract.rs`,
//! mirroring `ClientRewriteWireContractTest.kt`.
//!
//! The decoder is fail-closed: any structural anomaly returns `None`, matching
//! every `return null` in the Kotlin implementation.

pub mod announce;
pub mod file_packet;
pub mod fragment;
pub mod noise_payload;
pub mod packet;
pub mod padding;
pub mod sync;

pub use packet::{
    MessageType, Packet, WirePayload, FLAG_HAS_RECIPIENT, FLAG_HAS_ROUTE, FLAG_HAS_SIGNATURE,
    FLAG_IS_COMPRESSED, HEADER_SIZE_V1, HEADER_SIZE_V2, MAX_COMPRESSION_RATIO, MAX_PAYLOAD_LENGTH,
    RECIPIENT_ID_SIZE, SENDER_ID_SIZE, SIGNATURE_SIZE,
};
