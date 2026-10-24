//! TLV-encoded file transfer payload.
//! Byte-exact port of `BitchatFilePacket.kt` (mixed-width TLV format).
//!
//! - `0x01`: filename (UTF-8), 2-byte length
//! - `0x02`: file size, 2-byte length, 4-byte big-endian value
//! - `0x03`: mime type (UTF-8), 2-byte length
//! - `0x04`: content, 4-byte length (may appear multiple times; values concatenate)
//!
//! Unknown TLV tags are SKIPPED, not rejected (`case nil: continue` on iOS):
//! the tag list is a floor, not a ceiling. The length width is chosen by the
//! tag byte, including for unknown tags (2-byte).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePacket {
    pub file_name: String,
    pub file_size: i64,
    pub mime_type: String,
    pub content: Vec<u8>,
}

impl FilePacket {
    pub fn new(
        file_name: impl Into<String>,
        file_size: i64,
        mime_type: impl Into<String>,
        content: Vec<u8>,
    ) -> Self {
        Self {
            file_name: file_name.into(),
            file_size,
            mime_type: mime_type.into(),
            content,
        }
    }

    /// Encode with the deployed mixed-width layout: FILE_SIZE as a 4-byte
    /// value, CONTENT with a 4-byte length prefix.
    pub fn encode(&self) -> Option<Vec<u8>> {
        let name = self.file_name.as_bytes();
        let mime = self.mime_type.as_bytes();
        if name.len() > 0xFFFF || mime.len() > 0xFFFF {
            return None;
        }
        let mut out = Vec::new();
        put_tlv_16(&mut out, 0x01, name);
        out.push(0x02);
        out.extend_from_slice(&4u16.to_be_bytes());
        // Matches Java `putInt(fileSize.toInt())`: low 32 bits.
        out.extend_from_slice(&(self.file_size as i32).to_be_bytes());
        put_tlv_16(&mut out, 0x03, mime);
        out.push(0x04);
        out.extend_from_slice(&(self.content.len() as u32).to_be_bytes());
        out.extend_from_slice(&self.content);
        Some(out)
    }

    pub fn decode(data: &[u8]) -> Option<Self> {
        let mut offset = 0usize;
        let mut name = None;
        let mut size: Option<i64> = None;
        let mut mime = None;
        let mut content: Option<Vec<u8>> = None;

        while offset < data.len() {
            // Every TLV needs at least a type and its length field; a
            // truncated trailing header is rejected, matching iOS.
            let tlv_type = data[offset];
            let wide = tlv_type == 0x04;
            let len_bytes = if wide { 4 } else { 2 };
            if data.len() - offset < 1 + len_bytes {
                return None;
            }
            offset += 1;
            let length = if wide {
                u32::from_be_bytes([
                    data[offset],
                    data[offset + 1],
                    data[offset + 2],
                    data[offset + 3],
                ]) as usize
            } else {
                u16::from_be_bytes([data[offset], data[offset + 1]]) as usize
            };
            offset += len_bytes;
            if offset + length > data.len() {
                return None;
            }
            let value = &data[offset..offset + length];
            offset += length;

            match tlv_type {
                0x01 => name = Some(String::from_utf8_lossy(value).into_owned()),
                0x02 => {
                    if length != 4 {
                        return None;
                    }
                    size =
                        Some(i32::from_be_bytes([value[0], value[1], value[2], value[3]]) as i64);
                }
                0x03 => mime = Some(String::from_utf8_lossy(value).into_owned()),
                0x04 => match &mut content {
                    None => content = Some(value.to_vec()),
                    Some(existing) => existing.extend_from_slice(value),
                },
                _ => {} // unknown tag: skip its value
            }
        }

        let content = content?;
        Some(Self {
            file_name: name?,
            file_size: size.unwrap_or(content.len() as i64),
            mime_type: mime.unwrap_or_else(|| "application/octet-stream".to_string()),
            content,
        })
    }
}

fn put_tlv_16(out: &mut Vec<u8>, tlv_type: u8, value: &[u8]) {
    out.push(tlv_type);
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value);
}
