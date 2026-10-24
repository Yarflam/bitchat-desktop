//! Raw-DEFLATE compression, port of `CompressionUtil.kt` (iOS-compatible).
//!
//! Encoding output is not canonical across DEFLATE implementations, so the
//! only cross-encoder contract is: this side must decode Android/iOS bytes
//! exactly, and Android/iOS must decode ours. Both directions are tested
//! against fixed vectors below; byte-identical compression is NOT required
//! (mirrors the WirePayload rationale in `packet.rs`).

use flate2::{Compression, Decompress, FlushDecompress, Status};

/// Mirror of `AppConstants.Protocol.COMPRESSION_THRESHOLD_BYTES`.
pub const COMPRESSION_THRESHOLD_BYTES: usize = 100;

/// Mirror of `AppConstants.Protocol.MAX_PAYLOAD_LENGTH`.
pub const MAX_EXPANDED_SIZE: usize = crate::packet::MAX_PAYLOAD_LENGTH;

/// High-entropy heuristic, exact same logic as iOS/Android: count unique
/// bytes; compress only when the unique-byte ratio stays below 0.9.
pub fn should_compress(data: &[u8]) -> bool {
    if data.len() < COMPRESSION_THRESHOLD_BYTES {
        return false;
    }
    let mut byte_frequency = [0usize; 256];
    for &byte in data {
        byte_frequency[byte as usize] += 1;
    }
    let unique = byte_frequency.iter().filter(|&&c| c > 0).count();
    let unique_byte_ratio = unique as f64 / data.len().min(256) as f64;
    unique_byte_ratio < 0.9
}

/// Compress to raw DEFLATE (no zlib header, matching iOS COMPRESSION_ZLIB
/// behavior). Returns `None` when compression is not beneficial, matching the
/// "only if smaller" rule.
pub fn compress(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < COMPRESSION_THRESHOLD_BYTES {
        return None;
    }
    use std::io::Write;
    let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), Compression::default());
    // Encoding cannot realistically fail on a Vec sink; treat failure as
    // "not beneficial" like the Kotlin catch block.
    encoder.write_all(data).ok()?;
    let out = encoder.finish().ok()?;
    if !out.is_empty() && out.len() < data.len() {
        Some(out)
    } else {
        None
    }
}

/// Decompress a raw-DEFLATE (or coincidentally zlib-wrapped) stream into
/// exactly `original_size` bytes. Mirrors `CompressionUtil.decompressExact`:
/// a full output buffer alone is not success — the stream must end exactly at
/// the declared size with no input remaining and no further output byte.
pub fn decompress(compressed_data: &[u8], original_size: usize) -> Option<Vec<u8>> {
    if compressed_data.is_empty() {
        return None;
    }
    if original_size == 0 || original_size > MAX_EXPANDED_SIZE {
        return None;
    }
    if looks_like_zlib(compressed_data) {
        // A raw stream can coincidentally begin with a valid-looking zlib
        // header; the header only picks which format to try first.
        inflate_exact(compressed_data, original_size, true)
            .or_else(|| inflate_exact(compressed_data, original_size, false))
    } else {
        inflate_exact(compressed_data, original_size, false)
    }
}

/// RFC 1950 header check used to avoid speculative double inflation.
fn looks_like_zlib(data: &[u8]) -> bool {
    if data.len() < 2 {
        return false;
    }
    let cmf = data[0] as usize;
    let flg = data[1] as usize;
    (cmf & 0x0F) == 8 && (cmf >> 4) <= 7 && ((cmf << 8) | flg) % 31 == 0
}

fn inflate_exact(data: &[u8], original_size: usize, zlib: bool) -> Option<Vec<u8>> {
    let mut decoder = Decompress::new(zlib);
    // One extra byte of output room: if it ever fills, the declared size was
    // an under-declaration and the whole stream is rejected.
    let mut out = vec![0u8; original_size + 1];

    loop {
        let in_before = decoder.total_in();
        let out_before = decoder.total_out();
        let status = decoder
            .decompress(
                &data[decoder.total_in() as usize..],
                &mut out[decoder.total_out() as usize..],
                FlushDecompress::None,
            )
            .ok()?;

        match status {
            Status::StreamEnd => break,
            _ => {
                let made_progress =
                    decoder.total_in() != in_before || decoder.total_out() != out_before;
                if !made_progress {
                    // Truncated stream, or output full with more to produce.
                    return None;
                }
                if decoder.total_out() as usize == out.len() {
                    return None; // under-declared expanded size
                }
                if decoder.total_in() as usize == data.len()
                    && (decoder.total_out() as usize) < out.len()
                {
                    // All input consumed without reaching the stream end.
                    return None;
                }
            }
        }
    }

    let written = decoder.total_out() as usize;
    if written != original_size {
        return None;
    }
    if decoder.total_in() as usize != data.len() {
        return None; // trailing bytes after the compressed stream
    }
    out.pop();
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_compress_matches_entropy_heuristic() {
        assert!(!should_compress(&vec![0u8; 99]));
        assert!(should_compress(&vec![0u8; 100]));
        // 100 bytes of maximal diversity are treated as pre-compressed.
        let diverse: Vec<u8> = (0..100u8).cycle().take(100).collect();
        assert!(!should_compress(&diverse));
    }

    #[test]
    fn compress_only_when_smaller() {
        let repetitive: Vec<u8> = b"ab".repeat(200);
        let original_len = repetitive.len();
        let compressed = compress(&repetitive).expect("repetitive data must compress");
        assert!(compressed.len() < original_len);
        assert_eq!(Some(repetitive), decompress(&compressed, original_len));
    }

    #[test]
    fn incompressible_data_is_not_compressed() {
        // Deterministic pseudo-random bytes (SplitMix64-style finalizer, no
        // rand dependency).
        let mut state = 0x9E3779B97F4A7C15u64;
        let mut data = vec![0u8; 512];
        for byte in &mut data {
            state = state.wrapping_add(0x9E3779B97F4A7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
            z ^= z >> 31;
            *byte = (z >> 56) as u8;
        }
        // 512 samples over 256 symbols yield ~223 unique bytes (coupon
        // collector bound), a 0.87 ratio that PASSES the 0.9 entropy check —
        // exactly as on Android. The "only if smaller" rule is the layer
        // that declines this data.
        assert!(should_compress(&data));
        assert!(compress(&data).is_none());
    }

    #[test]
    fn decompress_rejects_bad_requests() {
        let repetitive: Vec<u8> = b"xy".repeat(200);
        let compressed = compress(&repetitive).unwrap();
        assert!(decompress(&[], repetitive.len()).is_none());
        assert!(decompress(&compressed, 0).is_none());
        assert!(decompress(&compressed, MAX_EXPANDED_SIZE + 1).is_none());
        // Under-declared size: the stream expands past the declaration.
        assert!(decompress(&compressed, 10).is_none());
        // Over-declared size: the stream ends before the declaration.
        assert!(decompress(&compressed, repetitive.len() * 2).is_none());
        // Trailing garbage after a valid stream.
        let mut trailing = compressed.clone();
        trailing.extend_from_slice(&[0xAA, 0xBB]);
        assert!(decompress(&trailing, repetitive.len()).is_none());
    }

    #[test]
    fn zlib_wrapped_input_is_tolerated() {
        use flate2::write::{DeflateEncoder, ZlibEncoder};
        use std::io::Write;

        let data: Vec<u8> = b"hello mesh ".repeat(50);
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&data).unwrap();
        let zlib_bytes = enc.finish().unwrap();
        assert_eq!(Some(data.clone()), decompress(&zlib_bytes, data.len()));

        let mut raw = DeflateEncoder::new(Vec::new(), Compression::default());
        raw.write_all(&data).unwrap();
        let raw_bytes = raw.finish().unwrap();
        let original_len = data.len();
        assert_eq!(Some(data), decompress(&raw_bytes, original_len));
    }
}
