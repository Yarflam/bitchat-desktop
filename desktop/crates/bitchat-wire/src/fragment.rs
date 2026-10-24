//! Fragment payload structure, 100% iOS-compatible.
//! Byte-exact port of `FragmentPayload.kt`.
//!
//! Wire layout (13-byte header):
//! - 8 bytes: fragment ID
//! - 2 bytes: index, big-endian, 0-based
//! - 2 bytes: total count, big-endian
//! - 1 byte: original message type
//! - remaining bytes: fragment data

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FragmentPayload {
    pub fragment_id: [u8; 8],
    pub index: u16,
    pub total: u16,
    pub original_type: u8,
    pub data: Vec<u8>,
}

pub const FRAGMENT_HEADER_SIZE: usize = 13;

impl FragmentPayload {
    pub fn new(
        fragment_id: [u8; 8],
        index: u16,
        total: u16,
        original_type: u8,
        data: Vec<u8>,
    ) -> Option<Self> {
        let f = Self {
            fragment_id,
            index,
            total,
            original_type,
            data,
        };
        f.is_valid().then_some(f)
    }

    /// Split `payload` into fragments for `original_type`, each carrying at
    /// most `max_data` bytes (use `MAX_FRAGMENT_SIZE` for BLE parity).
    pub fn split(
        fragment_id: [u8; 8],
        original_type: u8,
        payload: &[u8],
        max_data: usize,
    ) -> Vec<Self> {
        assert!(max_data > 0);
        let chunks: Vec<&[u8]> = payload.chunks(max_data).collect();
        let total = chunks.len().min(u16::MAX as usize) as u16;
        chunks
            .into_iter()
            .take(u16::MAX as usize)
            .enumerate()
            .map(|(i, chunk)| Self {
                fragment_id,
                index: i as u16,
                total,
                original_type,
                data: chunk.to_vec(),
            })
            .collect()
    }

    pub fn encode(&self) -> Vec<u8> {
        assert!(
            self.index < self.total,
            "fragment index must be below total"
        );
        let mut out = Vec::with_capacity(FRAGMENT_HEADER_SIZE + self.data.len());
        out.extend_from_slice(&self.fragment_id);
        out.extend_from_slice(&self.index.to_be_bytes());
        out.extend_from_slice(&self.total.to_be_bytes());
        out.push(self.original_type);
        out.extend_from_slice(&self.data);
        out
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() < FRAGMENT_HEADER_SIZE {
            return None;
        }
        let fragment_id: [u8; 8] = data[..8].try_into().ok()?;
        let index = u16::from_be_bytes([data[8], data[9]]);
        let total = u16::from_be_bytes([data[10], data[11]]);
        let original_type = data[12];
        Some(Self {
            fragment_id,
            index,
            total,
            original_type,
            data: data[FRAGMENT_HEADER_SIZE..].to_vec(),
        })
    }

    /// Constraint checks shared with Android: exact 8-byte ID, index below
    /// total, non-empty data.
    pub fn is_valid(&self) -> bool {
        self.index < self.total && !self.data.is_empty()
    }
}
