//! BitChat BLE link layer.
//!
//! - [`frame`]: pure Android-compatible GATT framing (one notification/write
//!   carries exactly one bitchat frame; Noise frames are padded). Unit-tested
//!   without any radio. The btleplug-based central node lands next.

pub mod frame;
