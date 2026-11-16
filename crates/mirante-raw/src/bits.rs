//! Module: mirante_raw::bits
//! Purpose: Bit reader, table-driven Huffman decoder and two-column predictor
//! for the Huffman-coded predictive RAW formats (Nikon, Pentax).
//! Created: 2026-11-16
//! Architecture: Shared by the maker crates (Nikon, Pentax, Canon, Olympus, Fujifilm).
//! Unlike JPEG, these streams have no byte stuffing or markers. Ported from
//! dcraw by Dave Coffin (getbithuff, make_decoder, the vpred/hpred predictor).

use mirante_core::DecodeError;

/// Longest code a lookup table may index; dcraw's trees stop at 16 bits.
const MAX_TABLE_BITS: u32 = 16;
const BUFFER_BITS: u32 = 64;

/// Reads MSB-first bits; past the end it feeds zeros and remembers that it did.
pub struct BitReader<'a> {
    data: &'a [u8],
    position: usize,
    buffer: u64,
    count: u32,
    /// How many of the last bits in `buffer` are zeros fed after the data.
    padding: u32,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0, buffer: 0, count: 0, padding: 0 }
    }

    fn refill(&mut self) {
        while self.count <= BUFFER_BITS - 8 {
            let byte = match self.data.get(self.position) {
                Some(&byte) => byte,
                None => {
                    self.padding += 8;
                    0
                }
            };
            self.position += 1;
            self.buffer |= u64::from(byte) << (BUFFER_BITS - 8 - self.count);
            self.count += 8;
        }
    }

    /// The next `bits` (0..=32) bits without consuming them.
    pub fn peek(&mut self, bits: u32) -> u32 {
        if bits == 0 {
            return 0;
        }
        if self.count < bits {
            self.refill();
        }
        (self.buffer >> (BUFFER_BITS - bits)) as u32
    }

    pub fn consume(&mut self, bits: u32) {
        self.buffer <<= bits;
        self.count -= bits;
    }

    pub fn read(&mut self, bits: u32) -> u32 {
        let value = self.peek(bits);
        self.consume(bits);
        value
    }

    /// Bits consumed so far, counting zeros fed past the end.
    pub fn consumed_bits(&self) -> u64 {
        self.position as u64 * 8 - u64::from(self.count)
    }

    /// True once more bits were consumed than the data held.
    pub fn overrun(&self) -> bool {
        self.count < self.padding
    }
}

/// A Huffman code resolved by one lookup of the next `bits` bits.
pub struct HuffmanTable {
    bits: u32,
    /// (code length << 8) | symbol, or 0 where no code starts.
    entries: Vec<u16>,
}

impl HuffmanTable {
    /// dcraw's tree format: 16 counts of codes per length (1..=16), then the
    /// symbols in code order. Codes are canonical; extra symbols are ignored.
    pub fn from_counts(spec: &[u8]) -> Result<Self, DecodeError> {
        let (counts, symbols) = spec
            .split_at_checked(16)
            .ok_or_else(|| DecodeError::corrupt("Huffman tree is too short"))?;
        let bits = counts.iter().rposition(|&count| count > 0).map_or(0, |last| last as u32 + 1);
        let mut entries = vec![0_u16; 1 << bits];
        let (mut next, mut symbols) = (0_usize, symbols.iter());
        for (index, &count) in counts.iter().enumerate().take(bits as usize) {
            let length = index as u32 + 1;
            for _ in 0..count {
                let symbol = *symbols
                    .next()
                    .ok_or_else(|| DecodeError::corrupt("Huffman tree lacks symbols"))?;
                let span = 1_usize << (bits - length);
                let Some(slots) = entries.get_mut(next..next + span) else {
                    return Err(DecodeError::corrupt("over-subscribed Huffman tree"));
                };
                slots.fill((length as u16) << 8 | u16::from(symbol));
                next += span;
            }
        }
        Ok(Self { bits, entries })
    }

    /// Codes given as (code left-aligned to `bits`, length); symbol = index.
    /// Codes that run past the table are dropped, as dcraw does.
    pub fn from_codes(bits: u32, codes: &[(u16, u8)]) -> Result<Self, DecodeError> {
        if !(1..=MAX_TABLE_BITS).contains(&bits) || codes.len() > 256 {
            return Err(DecodeError::corrupt("Huffman table size"));
        }
        let mut entries = vec![0_u16; 1 << bits];
        for (symbol, &(code, length)) in codes.iter().enumerate() {
            if length == 0 || u32::from(length) > bits {
                return Err(DecodeError::corrupt("Huffman code length"));
            }
            let start = usize::from(code);
            let span = 1_usize << (bits - u32::from(length));
            if let Some(slots) = entries.get_mut(start..start + span) {
                slots.fill(u16::from(length) << 8 | symbol as u16);
            }
        }
        Ok(Self { bits, entries })
    }

    pub fn decode(&self, reader: &mut BitReader<'_>) -> Result<u8, DecodeError> {
        let entry = self.entries[reader.peek(self.bits) as usize];
        let length = u32::from(entry >> 8);
        if length == 0 {
            return Err(DecodeError::corrupt("invalid Huffman code in RAW data"));
        }
        reader.consume(length);
        Ok(entry as u8)
    }
}

/// Reads a lossless-JPEG style difference of `length` bits (dcraw ljpeg_diff).
pub fn signed_difference(reader: &mut BitReader<'_>, length: u32) -> i32 {
    if length == 0 {
        return 0;
    }
    if length >= 16 {
        return -32768;
    }
    let diff = reader.read(length) as i32;
    if diff & (1 << (length - 1)) == 0 { diff - ((1 << length) - 1) } else { diff }
}

/// Each row's first two pixels predict from the same column two rows up;
/// the rest from the pixel two columns left. Arithmetic wraps at 16 bits.
pub struct Predictor {
    vertical: [[u16; 2]; 2],
    horizontal: [u16; 2],
}

impl Predictor {
    pub fn new(vertical: [[u16; 2]; 2]) -> Self {
        Self { vertical, horizontal: [0; 2] }
    }

    pub fn next(&mut self, row: usize, column: usize, diff: i32) -> u16 {
        let diff = diff as u16;
        if column < 2 {
            let start = &mut self.vertical[row & 1][column];
            *start = start.wrapping_add(diff);
            self.horizontal[column] = *start;
        } else {
            let value = &mut self.horizontal[column & 1];
            *value = value.wrapping_add(diff);
        }
        self.horizontal[column & 1]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example tree from dcraw's make_decoder comment.
    const TREE: [u8; 29] = [
        0, 1, 4, 2, 3, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x04, 0x03, 0x05, 0x06, 0x02, 0x07, 0x01,
        0x08, 0x09, 0x00, 0x0a, 0x0b, 0xff,
    ];

    #[test]
    fn decodes_canonical_tree() {
        let table = HuffmanTable::from_counts(&TREE).unwrap();
        // 00 | 010 | 1100 | 1111111, padded with zeros.
        let data = [0b0001_0110, 0b0111_1111, 0];
        let mut reader = BitReader::new(&data);
        let symbols: Vec<u8> = (0..4).map(|_| table.decode(&mut reader).unwrap()).collect();
        assert_eq!(symbols, [0x04, 0x03, 0x07, 0xff]);
        assert!(!reader.overrun());
    }

    #[test]
    fn rejects_over_subscribed_tree() {
        let mut tree = TREE;
        tree[1] = 3;
        assert!(HuffmanTable::from_counts(&tree).is_err());
    }

    #[test]
    fn from_codes_fills_left_aligned_ranges() {
        // Symbol 0 = "0" (1 bit), symbol 1 = "10", symbol 2 = "11", in a 4-bit table.
        let table = HuffmanTable::from_codes(4, &[(0, 1), (0b1000, 2), (0b1100, 2)]).unwrap();
        let mut reader = BitReader::new(&[0b0101_1000]);
        let symbols: Vec<u8> = (0..4).map(|_| table.decode(&mut reader).unwrap()).collect();
        assert_eq!(symbols, [0, 1, 2, 0]);
    }

    #[test]
    fn missing_codes_are_errors_and_overrun_is_seen() {
        let table = HuffmanTable::from_codes(2, &[(0, 1)]).unwrap();
        let mut reader = BitReader::new(&[0xFF]);
        assert!(table.decode(&mut reader).is_err());
        reader.read(8);
        assert!(!reader.overrun());
        reader.read(1);
        assert!(reader.overrun());
    }

    #[test]
    fn signed_differences_follow_jpeg() {
        let mut reader = BitReader::new(&[0b0110_0000]);
        assert_eq!(signed_difference(&mut reader, 1), -1);
        assert_eq!(signed_difference(&mut reader, 2), 3);
        assert_eq!(signed_difference(&mut reader, 0), 0);
    }

    #[test]
    fn predictor_uses_rows_then_columns() {
        let mut predictor = Predictor::new([[100, 200], [300, 400]]);
        assert_eq!(predictor.next(0, 0, 1), 101);
        assert_eq!(predictor.next(0, 1, -1), 199);
        assert_eq!(predictor.next(0, 2, 5), 106);
        assert_eq!(predictor.next(1, 0, 0), 300);
        assert_eq!(predictor.next(2, 0, 4), 105);
    }
}
