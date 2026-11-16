//! Module: mirante_codec::inflate
//! Purpose: DEFLATE and zlib decompression (RFC 1950/1951).
//! Created: 2026-11-16
//! Architecture: Used by PNG, TIFF (Deflate), PSD (ZIP) and DNG; owned by the PNG work.
//! Huffman codes decode through a 10-bit lookup table with a canonical slow path
//! for longer codes. The length/distance tables are shared with deflate.rs.

use crate::checksum::adler32;
use mirante_core::DecodeError;

/// Base match length and extra bits for length symbols 257..=285.
pub(crate) const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
pub(crate) const LENGTH_EXTRA: [u8; 29] =
    [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
/// Base distance and extra bits for distance symbols 0..=29.
pub(crate) const DISTANCE_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
pub(crate) const DISTANCE_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
/// Order in which a dynamic block lists the code-length code's lengths.
pub(crate) const CODE_LENGTH_ORDER: [usize; 19] =
    [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

pub(crate) const END_OF_BLOCK: u16 = 256;
pub(crate) const MAX_CODE_BITS: usize = 15;
const FAST_BITS: u32 = 10;
const FAST_MASK: u64 = (1 << FAST_BITS) - 1;
const ZLIB_METHOD_DEFLATE: u8 = 8;
const ZLIB_MAX_WINDOW_INFO: u8 = 7;
const ZLIB_PRESET_DICTIONARY: u8 = 0x20;

/// Code lengths of the fixed literal/length and distance codes (RFC 1951 3.2.6).
pub(crate) fn fixed_lengths() -> ([u8; 288], [u8; 30]) {
    let mut literal = [8u8; 288];
    literal[144..256].fill(9);
    literal[256..280].fill(7);
    (literal, [5u8; 30])
}

fn truncated() -> DecodeError {
    DecodeError::corrupt("compressed data ends early")
}

/// LSB-first bit reader; reads past the end yield zeros and are detected later.
struct BitReader<'a> {
    data: &'a [u8],
    position: usize,
    buffer: u64,
    count: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0, buffer: 0, count: 0 }
    }

    /// Guarantees at least 56 buffered bits.
    fn refill(&mut self) -> Result<(), DecodeError> {
        if let Some(word) = self.data.get(self.position..self.position + 8) {
            let word = u64::from_le_bytes(word.try_into().expect("slice has 8 bytes"));
            self.buffer |= word << self.count;
            self.position += (63 - self.count as usize) / 8;
            self.count |= 56;
            return Ok(());
        }
        while self.count <= 56 {
            // Padding beyond one buffer's worth can only mean the stream is truncated.
            if self.position >= self.data.len() + 8 {
                return Err(truncated());
            }
            let byte = self.data.get(self.position).copied().unwrap_or(0);
            self.buffer |= u64::from(byte) << self.count;
            self.position += 1;
            self.count += 8;
        }
        Ok(())
    }

    fn consume(&mut self, bits: u32) {
        self.buffer >>= bits;
        self.count -= bits;
    }

    /// Takes `bits` (at most 32) already buffered by `refill`.
    fn take(&mut self, bits: u32) -> u32 {
        let value = (self.buffer & ((1u64 << bits) - 1)) as u32;
        self.consume(bits);
        value
    }

    /// Bytes consumed so far, counting a partly used byte as consumed.
    fn consumed_bytes(&self) -> Result<usize, DecodeError> {
        let bits = self.position * 8 - self.count as usize;
        let bytes = bits.div_ceil(8);
        if bytes > self.data.len() { Err(truncated()) } else { Ok(bytes) }
    }

    /// Drops the partial byte and hands the byte position back for raw copies.
    fn align_to_byte(&mut self) -> Result<usize, DecodeError> {
        let position = self.consumed_bytes()?;
        *self = Self { position, ..Self::new(self.data) };
        Ok(position)
    }
}

/// A canonical Huffman code ready for decoding.
struct Huffman {
    /// Indexed by the next FAST_BITS input bits: `symbol << 4 | length`, 0 if longer.
    fast: Vec<u16>,
    counts: [u16; MAX_CODE_BITS + 1],
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Self, DecodeError> {
        let mut counts = [0u16; MAX_CODE_BITS + 1];
        for &length in lengths {
            counts[usize::from(length)] += 1;
        }
        counts[0] = 0;

        let mut available = 1i32;
        for &count in &counts[1..] {
            available = available * 2 - i32::from(count);
            if available < 0 {
                return Err(DecodeError::corrupt("over-subscribed Huffman code"));
            }
        }

        let mut next_code = [0u32; MAX_CODE_BITS + 2];
        let mut offsets = [0u16; MAX_CODE_BITS + 2];
        for length in 1..=MAX_CODE_BITS {
            next_code[length + 1] = (next_code[length] + u32::from(counts[length])) << 1;
            offsets[length + 1] = offsets[length] + counts[length];
        }

        let mut fast = vec![0u16; 1 << FAST_BITS];
        let mut symbols = vec![0u16; usize::from(offsets[MAX_CODE_BITS + 1])];
        for (symbol, &length) in lengths.iter().enumerate() {
            let length = usize::from(length);
            if length == 0 {
                continue;
            }
            symbols[usize::from(offsets[length])] = symbol as u16;
            offsets[length] += 1;
            let code = next_code[length];
            next_code[length] += 1;

            if length as u32 <= FAST_BITS {
                let reversed = code.reverse_bits() >> (32 - length);
                let entry = (symbol as u16) << 4 | length as u16;
                for index in (reversed as usize..fast.len()).step_by(1 << length) {
                    fast[index] = entry;
                }
            }
        }
        Ok(Self { fast, counts, symbols })
    }

    /// Needs at least MAX_CODE_BITS buffered bits.
    fn decode(&self, bits: &mut BitReader) -> Result<u16, DecodeError> {
        let entry = self.fast[(bits.buffer & FAST_MASK) as usize];
        if entry != 0 {
            bits.consume(u32::from(entry & 15));
            return Ok(entry >> 4);
        }
        self.decode_slow(bits)
    }

    /// Canonical decoding one bit at a time, for codes longer than FAST_BITS.
    fn decode_slow(&self, bits: &mut BitReader) -> Result<u16, DecodeError> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);

        for length in 1..=MAX_CODE_BITS {
            code |= ((bits.buffer >> (length - 1)) & 1) as i32;
            let count = i32::from(self.counts[length]);
            if code - first < count {
                bits.consume(length as u32);
                return Ok(self.symbols[(index + code - first) as usize]);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(DecodeError::corrupt("invalid Huffman code"))
    }
}

/// Reads a dynamic block's code lengths and builds its two codes.
fn read_dynamic_codes(bits: &mut BitReader) -> Result<(Huffman, Huffman), DecodeError> {
    bits.refill()?;
    let literal_count = bits.take(5) as usize + 257;
    let distance_count = bits.take(5) as usize + 1;
    let code_length_count = bits.take(4) as usize + 4;
    if literal_count > 286 || distance_count > 30 {
        return Err(DecodeError::corrupt("too many Huffman codes in a deflate block"));
    }

    let mut code_length_lengths = [0u8; 19];
    // Up to 57 bits, one more than a refill guarantees, so refill per entry.
    for &slot in &CODE_LENGTH_ORDER[..code_length_count] {
        bits.refill()?;
        code_length_lengths[slot] = bits.take(3) as u8;
    }
    let code_length_code = Huffman::new(&code_length_lengths)?;

    let total = literal_count + distance_count;
    let mut lengths = [0u8; 286 + 30];
    let mut filled = 0;
    while filled < total {
        bits.refill()?;
        let symbol = code_length_code.decode(bits)?;
        let (value, repeat) = match symbol {
            0..=15 => (symbol as u8, 1),
            16 => {
                let previous = filled.checked_sub(1).ok_or_else(|| {
                    DecodeError::corrupt("deflate length repeat with nothing to repeat")
                })?;
                (lengths[previous], 3 + bits.take(2) as usize)
            }
            17 => (0, 3 + bits.take(3) as usize),
            _ => (0, 11 + bits.take(7) as usize),
        };
        if filled + repeat > total {
            return Err(DecodeError::corrupt("deflate code lengths overflow"));
        }
        lengths[filled..filled + repeat].fill(value);
        filled += repeat;
    }

    if lengths[usize::from(END_OF_BLOCK)] == 0 {
        return Err(DecodeError::corrupt("deflate block has no end-of-block code"));
    }
    let literal = Huffman::new(&lengths[..literal_count])?;
    let distance = Huffman::new(&lengths[literal_count..total])?;
    Ok((literal, distance))
}

fn copy_match(output: &mut Vec<u8>, distance: usize, length: usize) {
    let start = output.len() - distance;
    if distance == 1 {
        let byte = output[start];
        output.resize(output.len() + length, byte);
        return;
    }
    // Overlapping matches repeat the last `distance` bytes, one period at a time.
    let mut copied = 0;
    while copied < length {
        let chunk = (length - copied).min(distance);
        output.extend_from_within(start + copied..start + copied + chunk);
        copied += chunk;
    }
}

fn exceeded(limit: usize) -> DecodeError {
    DecodeError::corrupt(format!("decompressed data exceeds the expected {limit} bytes"))
}

fn inflate_huffman_block(
    bits: &mut BitReader,
    output: &mut Vec<u8>,
    literal: &Huffman,
    distance: &Huffman,
    limit: usize,
) -> Result<(), DecodeError> {
    loop {
        // One refill covers the worst case: 15 + 5 + 15 + 13 bits.
        bits.refill()?;
        let symbol = literal.decode(bits)?;
        if symbol < END_OF_BLOCK {
            if output.len() >= limit {
                return Err(exceeded(limit));
            }
            output.push(symbol as u8);
            continue;
        }
        if symbol == END_OF_BLOCK {
            return Ok(());
        }

        let index = usize::from(symbol - 257);
        let (Some(&base), Some(&extra)) = (LENGTH_BASE.get(index), LENGTH_EXTRA.get(index)) else {
            return Err(DecodeError::corrupt("invalid deflate length symbol"));
        };
        let length = usize::from(base) + bits.take(u32::from(extra)) as usize;

        let index = usize::from(distance.decode(bits)?);
        let (Some(&base), Some(&extra)) = (DISTANCE_BASE.get(index), DISTANCE_EXTRA.get(index))
        else {
            return Err(DecodeError::corrupt("invalid deflate distance symbol"));
        };
        let back = usize::from(base) + bits.take(u32::from(extra)) as usize;

        if back > output.len() {
            return Err(DecodeError::corrupt("deflate match reaches before the data start"));
        }
        if output.len() + length > limit {
            return Err(exceeded(limit));
        }
        copy_match(output, back, length);
    }
}

/// Inflates and also returns how many input bytes the stream used.
fn inflate_stream(data: &[u8], limit: usize) -> Result<(Vec<u8>, usize), DecodeError> {
    let mut output = Vec::with_capacity(limit.min(data.len().saturating_mul(4)));
    let mut bits = BitReader::new(data);

    loop {
        bits.refill()?;
        let last = bits.take(1) == 1;
        match bits.take(2) {
            0 => {
                let start = bits.align_to_byte()?;
                let header = data.get(start..start + 4).ok_or_else(truncated)?;
                let length = u16::from_le_bytes([header[0], header[1]]);
                if length != !u16::from_le_bytes([header[2], header[3]]) {
                    return Err(DecodeError::corrupt("stored deflate block length mismatch"));
                }
                let end = start + 4 + usize::from(length);
                let stored = data.get(start + 4..end).ok_or_else(truncated)?;
                if output.len() + stored.len() > limit {
                    return Err(exceeded(limit));
                }
                output.extend_from_slice(stored);
                bits = BitReader { position: end, ..BitReader::new(data) };
            }
            1 => {
                let (literal, distance) = fixed_lengths();
                let literal = Huffman::new(&literal)?;
                let distance = Huffman::new(&distance)?;
                inflate_huffman_block(&mut bits, &mut output, &literal, &distance, limit)?;
            }
            2 => {
                let (literal, distance) = read_dynamic_codes(&mut bits)?;
                inflate_huffman_block(&mut bits, &mut output, &literal, &distance, limit)?;
            }
            _ => return Err(DecodeError::corrupt("reserved deflate block type")),
        }
        if last {
            return Ok((output, bits.consumed_bytes()?));
        }
    }
}

/// Decompresses a raw DEFLATE stream; output beyond `limit` bytes is an error.
pub fn inflate(data: &[u8], limit: usize) -> Result<Vec<u8>, DecodeError> {
    inflate_stream(data, limit).map(|(output, _)| output)
}

/// Decompresses a zlib stream (header, DEFLATE data, Adler-32), bounded by `limit`.
pub fn zlib_decompress(data: &[u8], limit: usize) -> Result<Vec<u8>, DecodeError> {
    let [method, flags, ..] = *data else {
        return Err(truncated());
    };
    if method & 0x0F != ZLIB_METHOD_DEFLATE || method >> 4 > ZLIB_MAX_WINDOW_INFO {
        return Err(DecodeError::unsupported("zlib stream is not DEFLATE"));
    }
    if (u16::from(method) << 8 | u16::from(flags)) % 31 != 0 {
        return Err(DecodeError::corrupt("zlib header check failed"));
    }
    if flags & ZLIB_PRESET_DICTIONARY != 0 {
        return Err(DecodeError::unsupported("zlib preset dictionary"));
    }

    let (output, used) = inflate_stream(&data[2..], limit)?;
    let trailer = data.get(2 + used..2 + used + 4).ok_or_else(truncated)?;
    let expected = u32::from_be_bytes(trailer.try_into().expect("slice has 4 bytes"));
    if adler32(&output) != expected {
        return Err(DecodeError::corrupt("zlib checksum mismatch"));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inflates_a_stored_block() {
        let data = [0x01, 0x03, 0x00, 0xFC, 0xFF, b'a', b'b', b'c'];
        assert_eq!(inflate(&data, 100).unwrap(), b"abc");
        assert!(inflate(&data, 2).is_err());
    }

    #[test]
    fn inflates_fixed_huffman_from_zlib() {
        // zlib.compress(b"hello hello hello hello")
        let data = [
            0x78, 0x9C, 0xCB, 0x48, 0xCD, 0xC9, 0xC9, 0x57, 0xC8, 0x40, 0x27, 0x01, 0x68, 0x03,
            0x08, 0xB1,
        ];
        assert_eq!(zlib_decompress(&data, 100).unwrap(), b"hello hello hello hello");
    }

    /// 100 pseudo-random bytes over a skewed alphabet, which zlib codes dynamically.
    fn skewed_text() -> Vec<u8> {
        let mut state = 1u32;
        (0..100)
            .map(|_| {
                state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345) & 0x7FFF_FFFF;
                b"aaaabbc d"[(state >> 16) as usize % 9]
            })
            .collect()
    }

    #[test]
    fn inflates_a_dynamic_block() {
        // zlib.compress(skewed_text(), 9)
        let data = [
            0x78, 0xDA, 0x3D, 0x8C, 0xC9, 0x0D, 0x00, 0x40, 0x08, 0x02, 0x5B, 0xA1, 0x35, 0x8E,
            0xFE, 0x6B, 0x58, 0xF0, 0xB1, 0x26, 0x44, 0x19, 0x09, 0x81, 0x49, 0x4E, 0x52, 0x45,
            0x9B, 0x1F, 0x0C, 0x05, 0xDD, 0xA8, 0xD7, 0x91, 0x7B, 0x29, 0x2E, 0x0B, 0x21, 0x59,
            0x66, 0xD4, 0x29, 0xF6, 0xA2, 0x59, 0xAC, 0xC7, 0x0A, 0xD8, 0x44, 0x3D, 0x1E, 0x2D,
            0x75, 0x23, 0xE3,
        ];
        assert_eq!(data[2] & 0x06, 0x04);
        assert_eq!(zlib_decompress(&data, 100).unwrap(), skewed_text());
        assert!(zlib_decompress(&data, 99).is_err());
    }

    #[test]
    fn rejects_bad_streams() {
        let good = [0x78, 0x9C, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01];
        assert_eq!(zlib_decompress(&good, 10).unwrap(), b"");
        let mut bad_checksum = good;
        bad_checksum[7] = 2;
        assert!(zlib_decompress(&bad_checksum, 10).is_err());
        assert!(zlib_decompress(&[0x78, 0xBB, 0x03, 0x00], 10).is_err());
        assert!(zlib_decompress(&good[..5], 10).is_err());
        // Reserved block type 3.
        assert!(inflate(&[0x07], 10).is_err());
        // Truncated input fails instead of decoding padding.
        assert!(inflate(&[0x04], 1 << 20).is_err());
    }
}
