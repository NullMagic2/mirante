//! Module: mirante_raw_nikon::compressed
//! Purpose: Nikon's Huffman-coded NEF compression (lossy and lossless, 12 and
//! 14 bits), driven by the maker note's NikonLinearizationTable (tag 0x0096).
//! Created: 2026-11-16
//! Architecture: Ported from dcraw by Dave Coffin (nikon_load_raw), with
//! LibRaw's handling of the 0x44/0x40 table version. Uses the shared bits module.

use mirante_codec::{ByteReader, Endian};
use mirante_core::DecodeError;
use mirante_raw::bits::{BitReader, HuffmanTable, Predictor};

/// dcraw's trees: 12-bit lossy, lossy after the split, lossless; then 14-bit.
/// The first tree's last symbol is the zero that pads dcraw's C array.
const TREES: [&[u8]; 6] = [
    &[0, 1, 5, 1, 1, 1, 1, 1, 1, 2, 0, 0, 0, 0, 0, 0, 5, 4, 3, 6, 2, 7, 1, 0, 8, 9, 11, 10, 12, 0],
    &[
        0, 1, 5, 1, 1, 1, 1, 1, 1, 2, 0, 0, 0, 0, 0, 0, 0x39, 0x5a, 0x38, 0x27, 0x16, 5, 4, 3, 2,
        1, 0, 11, 12, 12,
    ],
    &[0, 1, 4, 2, 3, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5, 4, 6, 3, 7, 2, 8, 1, 9, 0, 10, 11, 12],
    &[
        0, 1, 4, 3, 1, 1, 1, 1, 1, 2, 0, 0, 0, 0, 0, 0, 5, 6, 4, 7, 8, 3, 9, 2, 1, 0, 10, 11, 12,
        13, 14,
    ],
    &[
        0, 1, 5, 1, 1, 1, 1, 1, 1, 1, 2, 0, 0, 0, 0, 0, 8, 0x5c, 0x4b, 0x3a, 0x29, 7, 6, 5, 4, 3,
        2, 1, 0, 13, 14,
    ],
    &[
        0, 1, 4, 2, 2, 3, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 7, 6, 8, 5, 9, 4, 10, 3, 11, 12, 2, 0, 1,
        13, 14,
    ],
];
const LOSSLESS_TREE: usize = 2;
const FOURTEEN_BIT_TREES: usize = 3;
const VERSION_LOSSY: u8 = 0x44;
const VERSION_LOSSLESS: u8 = 0x46;
/// Versions with an extra 2110-byte block before the predictors.
const VERSION_EXTENDED: (u8, u8) = (0x49, 0x58);
const EXTENDED_SKIP: usize = 2110;
const SPLIT_OFFSET: usize = 562;
const CURVE_SIZE: usize = 0x10000;
const MAX_LISTED_CURVE: usize = 0x4001;
/// Predicted values index the curve after clamping to 14 bits.
const MAX_PREDICTED: i16 = 0x3fff;

/// What tag 0x0096 says about the compressed data.
#[derive(Debug)]
pub struct LinearizationTable {
    pub version: (u8, u8),
    pub initial: [[u16; 2]; 2],
    /// Maps decoded values to raw values; identity where the file gives none.
    pub curve: Vec<u16>,
    /// The row from which the second (post-split) tree applies; 0 for none.
    pub split: usize,
    pub tree: usize,
}

impl LinearizationTable {
    pub fn parse(bytes: &[u8], endian: Endian, bits: u32) -> Result<Self, DecodeError> {
        if bits != 12 && bits != 14 {
            return Err(DecodeError::unsupported(format!("{bits}-bit Nikon compression")));
        }
        let mut reader = ByteReader::new(bytes, endian);
        let version = (reader.u8()?, reader.u8()?);
        if version.0 == VERSION_EXTENDED.0 || version.1 == VERSION_EXTENDED.1 {
            reader.skip(EXTENDED_SKIP)?;
        }
        let initial = [[reader.u16()?, reader.u16()?], [reader.u16()?, reader.u16()?]];
        let mut max = (1_usize << bits) & 0x7fff;
        let points = usize::from(reader.u16()?);
        let mut step = if points > 1 { max / (points - 1) } else { 0 };
        let mut curve: Vec<u16> = (0..CURVE_SIZE).map(|value| value as u16).collect();
        let lossy = version.0 == VERSION_LOSSY && matches!(version.1, 0x20 | 0x40);
        if lossy && step > 0 && (version.1 == 0x20 || step > 3) {
            if version.1 == 0x40 {
                step /= 4;
                max /= 4;
            }
            // Control points every `step` values, linearly interpolated between.
            for index in 0..points {
                curve[index * step] = reader.u16()?;
            }
            for i in 0..max {
                let (base, offset) = (i - i % step, i % step);
                let low = u32::from(curve[base]) * (step - offset) as u32;
                let high = u32::from(curve[base + step]) * offset as u32;
                curve[i] = ((low + high) / step as u32) as u16;
            }
        } else if version.0 != VERSION_LOSSLESS && points <= MAX_LISTED_CURVE {
            for value in curve.iter_mut().take(points) {
                *value = reader.u16()?;
            }
        }
        let split = if lossy {
            let mut at = ByteReader::new(bytes, endian).at(SPLIT_OFFSET)?;
            usize::from(at.u16()?)
        } else {
            0
        };
        let tree = usize::from(version.0 == VERSION_LOSSLESS) * LOSSLESS_TREE
            + usize::from(bits == 14) * FOURTEEN_BIT_TREES;
        Ok(Self { version, initial, curve, split, tree })
    }

    /// Saturation: the largest value the curve gives for a `bits`-bit input.
    pub fn white(&self, bits: u32) -> u16 {
        let inputs = (1_usize << bits).min(CURVE_SIZE);
        self.curve[..inputs].iter().copied().max().unwrap_or(0)
    }
}

/// Decodes `width` x `height` photosites from Nikon's compressed stream.
pub fn decode(
    data: &[u8],
    width: usize,
    height: usize,
    table: &LinearizationTable,
) -> Result<Vec<u16>, DecodeError> {
    let pixels = DecodeError::check_size(width as u32, height as u32)?;
    // Every code is at least two bits long.
    if data.len().saturating_mul(4) < pixels {
        return Err(DecodeError::corrupt("Nikon compressed data is truncated"));
    }
    let tree = |index: usize| {
        let spec = TREES.get(index).ok_or_else(|| DecodeError::corrupt("Nikon Huffman tree"))?;
        HuffmanTable::from_counts(spec)
    };
    let mut huffman = tree(table.tree)?;
    let mut reader = BitReader::new(data);
    let mut predictor = Predictor::new(table.initial);
    let mut samples = vec![0_u16; pixels];
    for (row, line) in samples.chunks_exact_mut(width).enumerate() {
        if table.split > 0 && row == table.split {
            huffman = tree(table.tree + 1)?;
        }
        for (column, sample) in line.iter_mut().enumerate() {
            let symbol = u32::from(huffman.decode(&mut reader)?);
            let (length, shift) = (symbol & 15, symbol >> 4);
            let bits = reader.read(length.saturating_sub(shift)) as i32;
            let mut diff = (((bits << 1) + 1) << shift) >> 1;
            if length > 0 && diff & (1 << (length - 1)) == 0 {
                diff -= (1 << length) - i32::from(shift == 0);
            }
            let value = predictor.next(row, column, diff) as i16;
            *sample = table.curve[value.clamp(0, MAX_PREDICTED) as usize];
        }
    }
    if reader.overrun() {
        return Err(DecodeError::corrupt("Nikon compressed data is truncated"));
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_bytes(version: [u8; 2], points: &[u16]) -> Vec<u8> {
        let mut bytes = version.to_vec();
        for value in [10_u16, 20, 30, 40, points.len() as u16].iter().chain(points) {
            bytes.extend(value.to_le_bytes());
        }
        bytes.resize(SPLIT_OFFSET, 0);
        bytes.extend(7_u16.to_le_bytes());
        bytes
    }

    #[test]
    fn lossy_table_interpolates_curve_and_reads_split() {
        // Three points over 12 bits: step 2048.
        let bytes = table_bytes([0x44, 0x20], &[0, 1000, 4000]);
        let table = LinearizationTable::parse(&bytes, Endian::Little, 12).unwrap();
        assert_eq!(table.initial, [[10, 20], [30, 40]]);
        assert_eq!((table.curve[0], table.curve[1024], table.curve[2048]), (0, 500, 1000));
        assert_eq!(table.curve[3072], 2500);
        assert_eq!((table.split, table.tree, table.white(12)), (7, 0, 3998));
    }

    #[test]
    fn lossless_table_keeps_identity_curve() {
        let bytes = table_bytes([0x46, 0x30], &[5, 6]);
        let table = LinearizationTable::parse(&bytes, Endian::Little, 14).unwrap();
        assert_eq!(
            (table.curve[1], table.curve[0x3fff], table.split, table.tree),
            (1, 0x3fff, 0, 5)
        );
        assert_eq!(table.white(14), 0x3fff);
    }

    #[test]
    fn listed_curve_is_read_as_is() {
        let bytes = table_bytes([0x44, 0x10], &[9, 8, 7]);
        let table = LinearizationTable::parse(&bytes, Endian::Little, 12).unwrap();
        assert_eq!(&table.curve[..4], &[9, 8, 7, 3]);
        assert_eq!(table.split, 0);
    }

    #[test]
    fn decodes_lossless_stream() {
        let table = LinearizationTable {
            version: (0x46, 0x30),
            initial: [[100, 100], [100, 100]],
            curve: (0..=u16::MAX).collect(),
            split: 0,
            tree: LOSSLESS_TREE,
        };
        // Lossless 12-bit tree: symbol 0 is "11110", 2 is "1100" (two more bits).
        // Pixels: diff 0, then "1100" "11" = +3; the rest is padding.
        let data = [0b1111_0110, 0b0110_0000, 0, 0, 0, 0, 0, 0];
        let samples = decode(&data, 2, 1, &table).unwrap();
        assert_eq!(samples, [100, 103]);
    }

    #[test]
    fn truncated_stream_is_an_error() {
        let table = LinearizationTable {
            version: (0x46, 0x30),
            initial: [[0; 2]; 2],
            curve: (0..=u16::MAX).collect(),
            split: 0,
            tree: LOSSLESS_TREE,
        };
        // Every pixel needs at least two bits; 4 bytes hold 16 pixels at most.
        assert!(decode(&[0xFF; 4], 16, 1, &table).is_err());
    }
}
