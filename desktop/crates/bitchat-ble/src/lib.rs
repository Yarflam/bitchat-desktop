//! BitChat BLE link layer.
//!
//! - [`frame`]: pure Android-compatible GATT framing (one notification/write
//!   carries exactly one bitchat frame; Noise frames are padded). Unit-tested
//!   without any radio.
//! - [`central`]: a btleplug-based GATT central that discovers phones by the
//!   BitChat service advertisement and exchanges frames with them.

pub mod central;
pub mod frame;
#[cfg(windows)]
pub mod peripheral;

pub use btleplug::platform::PeripheralId;
