//! Module: mirante_raw_pentax::compressed
//! Purpose: Pentax Huffman compression (TIFF compression 65535), coded with
//! the table in maker note tag 0x0220.
//! Created: 2026-11-16
//! Architecture: Ported from dcraw by Dave Coffin (pentax_load_raw). The bit
//! reader, Huffman table and predictor are mirante_raw::bits.

use mirante_codec::{ByteReader, Endian};
use mirante_core::DecodeError;
use mirante_raw::bits::{BitReader, HuffmanTable, Predictor, signed_difference};

/// Codes are listed left-aligned to this many bits.
const TABLE_BITS: u32 = 12;
/// Bytes between the code count and the code list.
const TABLE_HEADER_SKIP: usize = 12;

/// Builds the decoder for tag 0x0220: a count, then the codes, then their lengths.
pub fn huffman_table(bytes: &[u8], endian: Endian) -> Result<HuffmanTable, DecodeError> {
    let mut reader = ByteReader::new(bytes, endian);
    let depth = usize::from(reader.u16()?.wrapping_add(12) & 15);
    reader.skip(TABLE_HEADER_SKIP)?;
    let codes = (0..depth).map(|_| reader.u16()).collect::<Result<Vec<_>, _>>()?;
    let lengths = reader.bytes(depth)?;
    let pairs: Vec<(u16, u8)> = codes.into_iter().zip(lengths.iter().copied()).collect();
    HuffmanTable::from_codes(TABLE_BITS, &pairs)
}

/// Decodes `width` x `height` photosites of `bits` bits each.
pub fn decode(
    data: &[u8],
    width: usize,
    height: usize,
    bits: u32,
    table: &HuffmanTable,
) -> Result<Vec<u16>, DecodeError> {
    let pixels = DecodeError::check_size(width as u32, height as u32)?;
    // Every code is at least one bit long.
    if data.len().saturating_mul(8) < pixels {
        return Err(DecodeError::corrupt("Pentax compressed data is truncated"));
    }
    let mut reader = BitReader::new(data);
    let mut predictor = Predictor::new([[0; 2]; 2]);
    let mut samples = vec![0_u16; pixels];
    let limit = 1_u32 << bits.min(16);
    for (row, line) in samples.chunks_exact_mut(width).enumerate() {
        for (column, sample) in line.iter_mut().enumerate() {
            let length = u32::from(table.decode(&mut reader)?);
            let diff = signed_difference(&mut reader, length);
            *sample = predictor.next(row, column, diff);
            if u32::from(*sample) >= limit {
                return Err(DecodeError::corrupt("Pentax sample exceeds its bit depth"));
            }
        }
    }
    if reader.overrun() {
        return Err(DecodeError::corrupt("Pentax compressed data is truncated"));
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Symbol = difference length: 0 is "0", 1 is "10", 2 is "11".
    fn table() -> HuffmanTable {
        let mut bytes = (3_u16.wrapping_sub(12) & 15).to_le_bytes().to_vec();
        bytes.extend([0; TABLE_HEADER_SKIP]);
        for code in [0x000_u16, 0x800, 0xC00] {
            bytes.extend(code.to_le_bytes());
        }
        bytes.extend([1, 2, 2]);
        huffman_table(&bytes, Endian::Little).unwrap()
    }

    #[test]
    fn decodes_differences_with_predictor() {
        // "11" "11" = +3, then "0" = 0, then "11" "01" = -2 from the pixel two left.
        let data = [0b1111_0110, 0b1000_0000, 0, 0];
        let samples = decode(&data, 3, 1, 14, &table()).unwrap();
        assert_eq!(samples, [3, 0, 1]);
    }

    #[test]
    fn rejects_values_beyond_bit_depth() {
        // "11" "00" = -3, which wraps to 65533.
        assert!(decode(&[0b1100_0000, 0], 1, 1, 14, &table()).is_err());
    }
}
