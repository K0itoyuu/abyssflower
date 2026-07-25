/// Zero-copy big-endian byte cursor for reading JVM class files.
/// All JVM binary formats are big-endian per the spec.
use crate::error::{DecompileError, Result};

pub struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    #[inline]
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    #[inline]
    pub fn position(&self) -> usize {
        self.pos
    }

    #[inline]
    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    #[inline]
    fn read_bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.data.len() {
            return Err(DecompileError::UnexpectedEof(self.pos));
        }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    #[inline]
    pub fn read_u8(&mut self) -> Result<u8> {
        if self.pos >= self.data.len() {
            return Err(DecompileError::UnexpectedEof(self.pos));
        }
        let b = self.data[self.pos];
        self.pos += 1;
        Ok(b)
    }

    #[inline]
    pub fn read_u16(&mut self) -> Result<u16> {
        let b = self.read_bytes(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    #[inline]
    pub fn read_u32(&mut self) -> Result<u32> {
        let b = self.read_bytes(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    #[inline]
    pub fn read_i16(&mut self) -> Result<i16> {
        Ok(self.read_u16()? as i16)
    }

    #[inline]
    pub fn read_i32(&mut self) -> Result<i32> {
        Ok(self.read_u32()? as i32)
    }

    #[inline]
    pub fn read_i64(&mut self) -> Result<i64> {
        let b = self.read_bytes(8)?;
        Ok(i64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    #[inline]
    pub fn read_u64(&mut self) -> Result<u64> {
        Ok(self.read_i64()? as u64)
    }

    #[inline]
    pub fn read_f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.read_u32()?))
    }

    #[inline]
    pub fn read_f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.read_u64()?))
    }

    /// Read a modified UTF-8 string as defined by the JVM spec (CONSTANT_Utf8).
    /// JVM MUTF-8 differs from standard UTF-8:
    /// - Null (U+0000) is encoded as 0xC0 0x80 (two bytes)
    /// - Supplementary characters use surrogate pairs in 3-byte encodings
    /// - Kotlin metadata d1 stores binary data in these strings
    pub fn read_mutf8(&mut self) -> Result<String> {
        let length = self.read_u16()? as usize;
        let bytes = self.read_bytes(length)?;
        // Fast path: pure ASCII (overwhelmingly common for class/method names)
        if bytes.iter().all(|&b| b < 0x80) {
            return Ok(unsafe { String::from_utf8_unchecked(bytes.to_vec()) });
        }
        // Decode MUTF-8 properly
        Ok(decode_mutf8(bytes))
    }

    /// Skip `n` bytes.
    #[inline]
    pub fn skip(&mut self, n: usize) -> Result<()> {
        if self.pos + n > self.data.len() {
            return Err(DecompileError::UnexpectedEof(self.pos));
        }
        self.pos += n;
        Ok(())
    }

    /// Return a sub-cursor of exactly `len` bytes and advance past them.
    pub fn sub_cursor(&mut self, len: usize) -> Result<Cursor<'a>> {
        if self.pos + len > self.data.len() {
            return Err(DecompileError::UnexpectedEof(self.pos));
        }
        let sub = Cursor::new(&self.data[self.pos..self.pos + len]);
        self.pos += len;
        Ok(sub)
    }

    /// Read all remaining bytes as a slice (zero-copy).
    #[inline]
    pub fn remaining_slice(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }
}

/// Decode JVM Modified UTF-8 bytes into a Rust String.
/// MUTF-8 encodes null as 0xC0 0x80 and supplementary chars as surrogate pairs.
/// For Kotlin metadata's d1 field, the bytes may contain arbitrary data encoded
/// as characters (each byte 0x01-0xFF maps to a char); invalid sequences are
/// preserved as replacement characters to not lose data.
fn decode_mutf8(bytes: &[u8]) -> String {
    let mut chars: Vec<u16> = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];
        if b == 0 {
            // Should not appear (null uses 0xC0 0x80) but handle gracefully
            chars.push(0);
            i += 1;
        } else if b < 0x80 {
            // Single byte: 0xxxxxxx
            chars.push(b as u16);
            i += 1;
        } else if b & 0xE0 == 0xC0 {
            // Two bytes: 110xxxxx 10xxxxxx
            if i + 1 < bytes.len() && bytes[i + 1] & 0xC0 == 0x80 {
                let ch = ((b as u16 & 0x1F) << 6) | (bytes[i + 1] as u16 & 0x3F);
                chars.push(ch);
                i += 2;
            } else {
                // Malformed — keep byte as-is
                chars.push(b as u16);
                i += 1;
            }
        } else if b & 0xF0 == 0xE0 {
            // Three bytes: 1110xxxx 10xxxxxx 10xxxxxx
            if i + 2 < bytes.len() && bytes[i + 1] & 0xC0 == 0x80 && bytes[i + 2] & 0xC0 == 0x80 {
                let ch = ((b as u16 & 0x0F) << 12)
                    | ((bytes[i + 1] as u16 & 0x3F) << 6)
                    | (bytes[i + 2] as u16 & 0x3F);
                chars.push(ch);
                i += 3;
            } else {
                chars.push(b as u16);
                i += 1;
            }
        } else {
            // Unknown/invalid lead byte — preserve as-is
            chars.push(b as u16);
            i += 1;
        }
    }

    String::from_utf16_lossy(&chars)
}
