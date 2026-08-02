//! Minimal protobuf wire-format decoder for Kotlin metadata.
//!
//! Supports only what's needed: varint, length-delimited messages, packed repeated.
//! No code generation; field dispatch is hand-written.

/// Wire types used in protobuf encoding.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WireType {
    Varint = 0,
    Fixed64 = 1,
    LengthDelimited = 2,
    Fixed32 = 5,
}

/// A single protobuf field as read from the wire.
#[derive(Debug)]
pub enum WireValue<'a> {
    Varint(u64),
    Fixed64(u64),
    Fixed32(u32),
    Bytes(&'a [u8]),
}

/// Low-level protobuf reader.
pub struct ProtoReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> ProtoReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.pos >= self.data.len()
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    /// Read a varint (LEB128 unsigned).
    pub fn read_varint(&mut self) -> Option<u64> {
        let mut result: u64 = 0;
        let mut shift = 0u32;
        loop {
            if self.pos >= self.data.len() {
                return None;
            }
            let b = self.data[self.pos];
            self.pos += 1;
            result |= ((b & 0x7F) as u64) << shift;
            if b & 0x80 == 0 {
                return Some(result);
            }
            shift += 7;
            if shift >= 64 {
                return None;
            }
        }
    }

    /// Read a signed varint (zigzag decoded).
    pub fn read_sint(&mut self) -> Option<i64> {
        let v = self.read_varint()?;
        Some(((v >> 1) as i64) ^ (-((v & 1) as i64)))
    }

    /// Read a varint as i32.
    pub fn read_int32(&mut self) -> Option<i32> {
        self.read_varint().map(|v| v as i32)
    }

    /// Read a varint as bool.
    pub fn read_bool(&mut self) -> Option<bool> {
        self.read_varint().map(|v| v != 0)
    }

    /// Read length-delimited bytes.
    pub fn read_bytes(&mut self) -> Option<&'a [u8]> {
        let len = self.read_varint()? as usize;
        if self.pos + len > self.data.len() {
            return None;
        }
        let slice = &self.data[self.pos..self.pos + len];
        self.pos += len;
        Some(slice)
    }

    /// Read a tag (field number + wire type).
    pub fn read_tag(&mut self) -> Option<(u32, WireType)> {
        let v = self.read_varint()?;
        let field_number = (v >> 3) as u32;
        let wire_type = match v & 0x07 {
            0 => WireType::Varint,
            1 => WireType::Fixed64,
            2 => WireType::LengthDelimited,
            5 => WireType::Fixed32,
            _ => return None, // unknown wire type, skip
        };
        Some((field_number, wire_type))
    }

    /// Skip a field value based on its wire type.
    pub fn skip(&mut self, wire_type: WireType) -> Option<()> {
        match wire_type {
            WireType::Varint => {
                self.read_varint()?;
            }
            WireType::Fixed64 => {
                if self.pos + 8 > self.data.len() {
                    return None;
                }
                self.pos += 8;
            }
            WireType::Fixed32 => {
                if self.pos + 4 > self.data.len() {
                    return None;
                }
                self.pos += 4;
            }
            WireType::LengthDelimited => {
                self.read_bytes()?;
            }
        }
        Some(())
    }

    /// Read a delimited sub-message (varint length prefix + bytes).
    pub fn read_message(&mut self) -> Option<ProtoReader<'a>> {
        let bytes = self.read_bytes()?;
        Some(ProtoReader::new(bytes))
    }

    /// Read packed repeated int32 from length-delimited bytes.
    pub fn read_packed_int32(bytes: &[u8]) -> Vec<i32> {
        let mut reader = ProtoReader::new(bytes);
        let mut result = Vec::new();
        while !reader.is_empty() {
            if let Some(v) = reader.read_varint() {
                result.push(v as i32);
            } else {
                break;
            }
        }
        result
    }
}

// ── BitEncoding: decode d1 String[] → raw protobuf bytes ─────────────────

/// Decode the `d1` field of @kotlin/Metadata into raw protobuf bytes.
/// Implements Kotlin's BitEncoding algorithm.
///
/// In the `\0`-marker format, each Java char in d1 maps directly to one byte
/// (char code 0-255). Since our MUTF-8 decoder produces Rust Strings (UTF-8),
/// we need to extract the char values (as u16) to recover the original bytes.
pub fn decode_bit_encoding(d1: &[String]) -> Vec<u8> {
    if d1.is_empty() {
        return Vec::new();
    }

    let first_char = d1[0].chars().next().unwrap_or('\x01');

    if first_char == '\0' {
        // Direct mode (newer format): each char after the marker is one byte.
        // The chars are code points 0-255, directly mapping to bytes.
        let mut result = Vec::new();
        for (i, s) in d1.iter().enumerate() {
            let chars: Vec<char> = s.chars().collect();
            let start = if i == 0 { 1 } else { 0 }; // skip marker on first string
            for &ch in &chars[start..] {
                result.push((ch as u32 & 0xFF) as u8);
            }
        }
        return result;
    }

    // 8-to-7 decode (legacy and 0xFFFF-marked format)
    let skip_first = first_char == '\u{FFFF}';

    // Collect all chars as their code point values (each maps to a byte)
    let mut bytes: Vec<u8> = Vec::new();
    for (i, s) in d1.iter().enumerate() {
        let chars: Vec<char> = s.chars().collect();
        let start = if i == 0 && skip_first { 1 } else { 0 };
        for &ch in &chars[start..] {
            bytes.push((ch as u32 & 0xFF) as u8);
        }
    }

    // Add 0x7F modulo to each byte
    for b in bytes.iter_mut() {
        *b = (*b as u16 + 0x7F) as u8 & 0x7F;
    }

    // 7-to-8 decode: each input byte is 7 bits
    let out_len = 7 * bytes.len() / 8;
    let mut result = vec![0u8; out_len];
    let mut bit_pos = 0usize;

    for &b in &bytes {
        for bit_idx in 0..7 {
            let bit = (b >> bit_idx) & 1;
            let out_byte_idx = bit_pos / 8;
            let out_bit_idx = bit_pos % 8;
            if out_byte_idx < result.len() {
                result[out_byte_idx] |= bit << out_bit_idx;
            }
            bit_pos += 1;
        }
    }

    result
}
