//! Module: mirante_format_jpeg::huffman
//! Purpose: Canonical JPEG Huffman codes and the bit reader for entropy-coded data.
//! Created: 2026-11-16
//! Architecture: `canonical_codes` serves both the decoder tables here and the
//! encoder. The bit reader stops at markers and then feeds zeros, counting them so
//! callers can tell real data from padding when a file is truncated.

use mirante_core::DecodeError;

/// Codes up to this length resolve with one table lookup.
const FAST_BITS: usize = 9;
const MAX_CODE_LENGTH: usize = 16;

/// The (code, length) of each value, in the order the values are listed.
pub fn canonical_codes(counts: &[u8; 16]) -> Result<Vec<(u16, u8)>, DecodeError> {
    let mut codes = Vec::new();
    let mut code = 0_u32;

    for (index, &count) in counts.iter().enumerate() {
        let length = index + 1;
        for _ in 0..count {
            if code >= 1 << length {
                return Err(DecodeError::corrupt("over-subscribed JPEG Huffman table"));
            }
            codes.push((code as u16, length as u8));
            code += 1;
        }
        code <<= 1;
    }
    Ok(codes)
}

pub struct HuffmanTable {
    /// Indexed by the next FAST_BITS bits: (length << 8) | value, or 0 if longer.
    fast: [u16; 1 << FAST_BITS],
    /// Largest code of each length, or -1 when that length has no codes.
    max_code: [i32; MAX_CODE_LENGTH + 1],
    /// Added to a code of each length to find its index in `values`.
    value_offset: [i32; MAX_CODE_LENGTH + 1],
    values: Vec<u8>,
}

impl HuffmanTable {
    pub fn new(counts: &[u8; 16], values: &[u8]) -> Result<Self, DecodeError> {
        let codes = canonical_codes(counts)?;
        let mut table = Self {
            fast: [0; 1 << FAST_BITS],
            max_code: [-1; MAX_CODE_LENGTH + 1],
            value_offset: [0; MAX_CODE_LENGTH + 1],
            values: values.to_vec(),
        };
        let mut first = 0;

        for (index, &count) in counts.iter().enumerate() {
            let count = usize::from(count);
            if count > 0 {
                let length = index + 1;
                table.value_offset[length] = first as i32 - i32::from(codes[first].0);
                table.max_code[length] = i32::from(codes[first + count - 1].0);
            }
            first += count;
        }
        for (&(code, length), &value) in codes.iter().zip(values) {
            let length = usize::from(length);
            if length <= FAST_BITS {
                let shift = FAST_BITS - length;
                let start = usize::from(code) << shift;
                let entry = (length as u16) << 8 | u16::from(value);
                table.fast[start..start + (1 << shift)].fill(entry);
            }
        }
        Ok(table)
    }
}

pub struct BitReader<'a> {
    data: &'a [u8],
    /// The first byte not yet loaded into `buffer`.
    position: usize,
    /// Unread bits, most significant first.
    buffer: u64,
    count: u32,
    /// How many of the last bits in `buffer` are zeros fed after the data ended.
    padding: u32,
    exhausted: bool,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8], position: usize) -> Self {
        Self { data, position, buffer: 0, count: 0, padding: 0, exhausted: false }
    }

    pub fn position(&self) -> usize {
        self.position
    }

    /// True once more bits were consumed than the entropy-coded data held.
    pub fn overrun(&self) -> bool {
        self.count < self.padding
    }

    fn refill(&mut self) {
        while self.count <= 56 {
            let byte = self.next_byte();
            self.buffer |= u64::from(byte) << (56 - self.count);
            self.count += 8;
        }
    }

    fn next_byte(&mut self) -> u8 {
        if !self.exhausted {
            match self.data.get(self.position) {
                Some(0xFF) if self.data.get(self.position + 1) == Some(&0) => {
                    self.position += 2;
                    return 0xFF;
                }
                Some(&byte) if byte != 0xFF => {
                    self.position += 1;
                    return byte;
                }
                _ => self.exhausted = true,
            }
        }
        self.padding = self.padding.saturating_add(8);
        0
    }

    /// Reads `length` bits (at most 16) as an unsigned number.
    pub fn bits(&mut self, length: u32) -> u32 {
        if length == 0 {
            return 0;
        }
        if self.count < length {
            self.refill();
        }
        let value = (self.buffer >> (64 - length)) as u32;
        self.buffer <<= length;
        self.count -= length;
        value
    }

    pub fn bit(&mut self) -> bool {
        self.bits(1) == 1
    }

    /// Reads a `length`-bit magnitude and applies JPEG's EXTEND sign rule (F.2.2.1).
    pub fn receive_extend(&mut self, length: u32) -> i32 {
        if length == 0 {
            return 0;
        }
        let value = self.bits(length) as i32;
        if value < 1 << (length - 1) { value - (1 << length) + 1 } else { value }
    }

    pub fn decode(&mut self, table: &HuffmanTable) -> Result<u8, DecodeError> {
        if self.count < MAX_CODE_LENGTH as u32 {
            self.refill();
        }
        let peek = (self.buffer >> (64 - MAX_CODE_LENGTH)) as usize;
        let fast = table.fast[peek >> (MAX_CODE_LENGTH - FAST_BITS)];

        if fast != 0 {
            self.bits(u32::from(fast >> 8));
            return Ok(fast as u8);
        }
        for length in FAST_BITS + 1..=MAX_CODE_LENGTH {
            let code = (peek >> (MAX_CODE_LENGTH - length)) as i32;
            if code <= table.max_code[length] {
                self.bits(length as u32);
                let index = code + table.value_offset[length];
                return usize::try_from(index)
                    .ok()
                    .and_then(|index| table.values.get(index).copied())
                    .ok_or_else(|| DecodeError::corrupt("bad JPEG Huffman code"));
            }
        }
        Err(DecodeError::corrupt("bad JPEG Huffman code"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_short_and_long_codes() {
        // One 1-bit code, then codes of length 2 and 12 (Kraft sum below 1).
        let mut counts = [0; 16];
        counts[0] = 1;
        counts[1] = 1;
        counts[11] = 1;
        let table = HuffmanTable::new(&counts, &[7, 8, 9]).unwrap();
        // 0 | 10 | 110000000000 | 1110 padding.
        let data = [0b0101_1000, 0b0000_0001, 0b1100_0000];
        let mut reader = BitReader::new(&data, 0);
        assert_eq!(reader.decode(&table).unwrap(), 7);
        assert_eq!(reader.decode(&table).unwrap(), 8);
        assert_eq!(reader.decode(&table).unwrap(), 9);
        assert!(!reader.overrun());
    }

    #[test]
    fn unstuffs_and_stops_at_markers() {
        let data = [0xFF, 0x00, 0xA5, 0xFF, 0xD9];
        let mut reader = BitReader::new(&data, 0);
        assert_eq!(reader.bits(16), 0xFFA5);
        assert!(!reader.overrun());
        assert_eq!(reader.bits(1), 0);
        assert!(reader.overrun());
        assert_eq!(reader.position(), 3);
    }

    #[test]
    fn extends_negative_values() {
        let mut reader = BitReader::new(&[0b0101_0000], 0);
        assert_eq!(reader.receive_extend(3), -5);
        assert_eq!(reader.receive_extend(1), 1);
    }
}
