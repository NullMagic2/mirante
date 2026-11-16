//! Module: mirante_raw_fuji::uncompressed
//! Purpose: Reads uncompressed RAF photosites: 16-bit words or packed samples.
//! Created: 2026-11-16
//! Architecture: The data size tells the layouts apart, as in dcraw's
//! unpacked_load_raw and packed_load_raw (Dave Coffin). Packed rows start on
//! byte boundaries and use mirante_raw's MSB-first PackedBits.

use crate::header::Size;
use mirante_codec::Endian;
use mirante_core::DecodeError;
use mirante_raw::unpack::PackedBits;

/// One 16-bit word per photosite, in the section's byte order.
pub fn unpacked(bytes: &[u8], size: Size, endian: Endian) -> Result<Vec<u16>, DecodeError> {
    let words = bytes
        .get(..size.width * size.height * 2)
        .ok_or_else(|| DecodeError::corrupt("RAF raw data is truncated"))?;
    Ok(words
        .chunks_exact(2)
        .map(|pair| match endian {
            Endian::Little => u16::from_le_bytes([pair[0], pair[1]]),
            Endian::Big => u16::from_be_bytes([pair[0], pair[1]]),
        })
        .collect())
}

pub fn packed_row_bytes(size: Size, bits: u32) -> usize {
    (size.width * bits as usize).div_ceil(8)
}

/// `bits`-bit samples packed most significant bit first, rows byte-aligned.
pub fn packed(bytes: &[u8], size: Size, bits: u32) -> Result<Vec<u16>, DecodeError> {
    let row_bytes = packed_row_bytes(size, bits);
    let rows = bytes
        .get(..row_bytes * size.height)
        .ok_or_else(|| DecodeError::corrupt("RAF raw data is truncated"))?;
    let mut samples = Vec::with_capacity(size.width * size.height);
    for row in rows.chunks_exact(row_bytes) {
        samples.extend(PackedBits::new(row, bits).take(size.width));
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_words_in_either_byte_order() {
        let size = Size { height: 1, width: 2 };
        let bytes = [0x12, 0x34, 0x56, 0x78];
        assert_eq!(unpacked(&bytes, size, Endian::Little).unwrap(), [0x3412, 0x7856]);
        assert_eq!(unpacked(&bytes, size, Endian::Big).unwrap(), [0x1234, 0x5678]);
        assert!(unpacked(&bytes[..3], size, Endian::Big).is_err());
    }

    #[test]
    fn reads_packed_fourteen_bit_rows() {
        // Two rows of two 14-bit samples: 0x3FFF, 0x0001 and 0x2AAA, 0x1555.
        let row = |a: u32, b: u32| {
            let bits = (a << 14 | b) << 4;
            bits.to_be_bytes()[..4].to_vec()
        };
        let bytes = [row(0x3FFF, 1), row(0x2AAA, 0x1555)].concat();
        let size = Size { height: 2, width: 2 };
        assert_eq!(packed(&bytes, size, 14).unwrap(), [0x3FFF, 1, 0x2AAA, 0x1555]);
        assert!(packed(&bytes[..7], size, 14).is_err());
    }
}
