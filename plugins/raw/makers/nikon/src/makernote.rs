//! Module: mirante_raw_nikon::makernote
//! Purpose: Reads the Nikon maker note fields that RAW decoding needs: white
//! balance (0x000C), black level (0x003D) and the linearisation table (0x0096).
//! Created: 2026-11-16
//! Architecture: The maker note is a TIFF of its own after "Nikon\0" and a
//! version, so mirante_codec's Tiff parses it. Field meanings follow dcraw.

use mirante_codec::Endian;
use mirante_codec::tiff::Tiff;
use mirante_core::DecodeError;

const SIGNATURE: &[u8] = b"Nikon\0";
/// "Nikon\0", two version bytes and two reserved bytes precede the TIFF header.
const TIFF_START: usize = 10;
const WB_RB_LEVELS: u16 = 0x000C;
const BLACK_LEVEL: u16 = 0x003D;
const LINEARIZATION_TABLE: u16 = 0x0096;

#[derive(Debug)]
pub struct MakerNote<'a> {
    /// R, G, B multipliers.
    pub white_balance: Option<[f64; 3]>,
    /// Black at 14 bits, in dcraw's order: R, G on red rows, G on blue rows, B.
    pub black: Option<[u16; 4]>,
    pub linearization: Option<&'a [u8]>,
    pub endian: Endian,
}

impl<'a> MakerNote<'a> {
    /// Notes without the "Nikon\0" TIFF form (very old cameras) yield nothing.
    pub fn parse(note: &'a [u8]) -> Result<Self, DecodeError> {
        let mut found =
            Self { white_balance: None, black: None, linearization: None, endian: Endian::Little };
        let Some(tiff_bytes) = note.strip_prefix(SIGNATURE).and_then(|_| note.get(TIFF_START..))
        else {
            return Ok(found);
        };
        let tiff = Tiff::parse(tiff_bytes)?;
        let Some(ifd) = tiff.ifds.first() else {
            return Ok(found);
        };
        found.endian = tiff.endian();
        if let Some(entry) = ifd.get(WB_RB_LEVELS) {
            // Red, blue, then green levels; dcraw stores them as R, B, G.
            if let [red, blue, green, ..] = tiff.numbers(entry)?[..] {
                let levels = [red, green, blue];
                if levels.iter().all(|level| level.is_finite() && *level > 0.0) {
                    found.white_balance = Some(levels);
                }
            }
        }
        if let Some(entry) = ifd.get(BLACK_LEVEL) {
            if let [red, green_red, green_blue, blue] = tiff.unsigned(entry)?[..] {
                let levels =
                    [red, green_red, green_blue, blue].map(|level| level.min(0xFFFF) as u16);
                found.black = Some(levels);
            }
        }
        found.linearization =
            ifd.get(LINEARIZATION_TABLE).map(|entry| tiff.raw_bytes(entry)).transpose()?;
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A maker note whose TIFF holds WB_RBLevels, BlackLevel and a 4-byte table.
    fn note() -> Vec<u8> {
        let mut tiff = b"II*\0".to_vec();
        tiff.extend(8_u32.to_le_bytes());
        tiff.extend(3_u16.to_le_bytes());
        let rationals = 8 + 2 + 3 * 12 + 4;
        tiff.extend([0x0C, 0, 5, 0, 4, 0, 0, 0]);
        tiff.extend((rationals as u32).to_le_bytes());
        tiff.extend([0x3D, 0, 3, 0, 4, 0, 0, 0]);
        tiff.extend((rationals as u32 + 32).to_le_bytes());
        tiff.extend([0x96, 0, 7, 0, 4, 0, 0, 0, 0x46, 0x30, 1, 2]);
        tiff.extend(0_u32.to_le_bytes());
        for (numerator, denominator) in [(512_u32, 256_u32), (300, 256), (256, 256), (256, 256)] {
            tiff.extend(numerator.to_le_bytes());
            tiff.extend(denominator.to_le_bytes());
        }
        for level in [600_u16, 601, 602, 603] {
            tiff.extend(level.to_le_bytes());
        }
        let mut note = b"Nikon\0\x02\x11\0\0".to_vec();
        note.extend(tiff);
        note
    }

    #[test]
    fn reads_white_balance_black_and_table() {
        let bytes = note();
        let note = MakerNote::parse(&bytes).unwrap();
        assert_eq!(note.white_balance, Some([2.0, 1.0, 300.0 / 256.0]));
        assert_eq!(note.black, Some([600, 601, 602, 603]));
        assert_eq!(note.linearization, Some(&[0x46, 0x30, 1, 2][..]));
    }

    #[test]
    fn ignores_notes_without_signature() {
        let note = MakerNote::parse(b"OLYMPUS\0II").unwrap();
        assert!(note.white_balance.is_none() && note.linearization.is_none());
    }
}
