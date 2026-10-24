//! Privacy-preserving PKCS#7 padding, exact port of `MessagePadding.kt`
//! (iOS-compatible selective padding for BLE Noise frames).

/// Standard block sizes, same as Android/iOS.
const BLOCK_SIZES: [usize; 4] = [256, 512, 1024, 2048];

/// Smallest standard block that fits `data_size` plus the encryption overhead
/// allowance (~16 bytes). Oversized input is returned unpadded.
pub fn optimal_block_size(data_size: usize) -> usize {
    let total_size = data_size.saturating_add(16);
    for block_size in BLOCK_SIZES {
        if total_size <= block_size {
            return block_size;
        }
    }
    data_size
}

/// Pad `data` to `target_size` with PKCS#7 (every pad byte equals the pad
/// length). Returns `data` unchanged when no valid padding applies:
/// `data_size >= target_size` or a pad length above 255 (single-byte marker).
pub fn pad(data: &[u8], target_size: usize) -> Vec<u8> {
    if data.len() >= target_size {
        return data.to_vec();
    }
    let padding_needed = target_size - data.len();
    if padding_needed == 0 || padding_needed > 255 {
        return data.to_vec();
    }
    let mut result = Vec::with_capacity(target_size);
    result.extend_from_slice(data);
    result.extend(std::iter::repeat_n(padding_needed as u8, padding_needed));
    result
}

/// Strict PKCS#7 unpad. Returns the original bytes when the trailing bytes do
/// not form valid padding (wrong marker, mismatched pad bytes, or out of
/// range), matching the iOS/Android behavior of leaving data untouched.
pub fn unpad(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return data.to_vec();
    }
    let padding_length = data[data.len() - 1] as usize;
    if padding_length == 0 || padding_length > data.len() {
        return data.to_vec();
    }
    let start = data.len() - padding_length;
    if data[start..].iter().any(|&b| b != data[data.len() - 1]) {
        return data.to_vec();
    }
    data[..start].to_vec()
}
