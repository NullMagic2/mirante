//! Module: mirante_raw_pentax::makernote
//! Purpose: Reads the Pentax maker note fields that RAW decoding needs: black
//! level (0x0200), white balance (0x0201) and the Huffman table (0x0220).
//! Created: 2026-11-16
//! Architecture: "PENTAX \0" notes count offsets from the note's start, older
//! "AOC\0" notes from the file's start (dcraw's parse_makernote). Both carry
//! their own byte order and are read with mirante_codec's headerless Tiff.

use mirante_codec::Endian;
use mirante_codec::tiff::Tiff;
use mirante_core::DecodeError;

const PENTAX_SIGNATURE: &[u8] = b"PENTAX \0";
const AOC_SIGNATURE: &[u8] = b"AOC\0";
const BLACK_POINT: u16 = 0x0200;
const WHITE_POINT: u16 = 0x0201;
const HUFFMAN_TABLE: u16 = 0x0220;

#[derive(Debug, Default)]
pub struct MakerNote<'a> {
    /// In dcraw's order: R, G on red rows, G on blue rows, B.
    pub black: Option<[f32; 4]>,
    /// R, G, B multipliers.
    pub white_balance: Option<[f64; 3]>,
    pub huffman: Option<&'a [u8]>,
    pub endian: Option<Endian>,
}

impl<'a> MakerNote<'a> {
    /// `note` must be a sub-slice of `file`. Unknown note layouts yield nothing.
    pub fn parse(file: &'a [u8], note: &'a [u8]) -> Result<Self, DecodeError> {
        let (data, endian_at, ifd_at) = if note.starts_with(PENTAX_SIGNATURE) {
            (note, PENTAX_SIGNATURE.len(), PENTAX_SIGNATURE.len() + 2)
        } else if note.starts_with(AOC_SIGNATURE) {
            let start = (note.as_ptr() as usize).wrapping_sub(file.as_ptr() as usize);
            if start >= file.len() {
                return Err(DecodeError::corrupt("Pentax maker note outside the file"));
            }
            (file, start + AOC_SIGNATURE.len(), start + AOC_SIGNATURE.len() + 2)
        } else {
            return Ok(Self::default());
        };
        let endian = match data.get(endian_at..endian_at + 2) {
            Some(b"II") => Endian::Little,
            Some(b"MM") => Endian::Big,
            _ => return Err(DecodeError::corrupt("Pentax maker note byte order")),
        };
        let tiff = Tiff::headerless(data, endian);
        let (ifd, _) = tiff.read_ifd(ifd_at)?;
        let four = |tag| -> Result<Option<[u32; 4]>, DecodeError> {
            match ifd.get(tag) {
                Some(entry) => Ok(<[u32; 4]>::try_from(tiff.unsigned(entry)?).ok()),
                None => Ok(None),
            }
        };
        let black = four(BLACK_POINT)?.map(|levels| levels.map(|level| level as f32));
        let white_balance = four(WHITE_POINT)?
            .filter(|levels| levels.iter().all(|&level| level > 0))
            .map(|[red, green, _, blue]| [red, green, blue].map(f64::from));
        let huffman = ifd.get(HUFFMAN_TABLE).map(|entry| tiff.raw_bytes(entry)).transpose()?;
        Ok(Self { black, white_balance, huffman, endian: Some(endian) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "PENTAX \0II", then an IFD at 10 with BlackPoint, WhitePoint and a table.
    fn note() -> Vec<u8> {
        let mut note = b"PENTAX \0II".to_vec();
        note.extend(3_u16.to_le_bytes());
        let values = 10 + 2 + 3 * 12 + 4;
        note.extend([0x00, 0x02, 3, 0, 4, 0, 0, 0]);
        note.extend((values as u32).to_le_bytes());
        note.extend([0x01, 0x02, 3, 0, 4, 0, 0, 0]);
        note.extend((values as u32 + 8).to_le_bytes());
        note.extend([0x20, 0x02, 7, 0, 2, 0, 0, 0, 0xAB, 0xCD, 0, 0]);
        note.extend(0_u32.to_le_bytes());
        for value in [64_u16, 65, 66, 67, 17600, 8192, 8192, 13248] {
            note.extend(value.to_le_bytes());
        }
        note
    }

    #[test]
    fn reads_pentax_note() {
        let bytes = note();
        let note = MakerNote::parse(&bytes, &bytes).unwrap();
        assert_eq!(note.black, Some([64.0, 65.0, 66.0, 67.0]));
        assert_eq!(note.white_balance, Some([17600.0, 8192.0, 13248.0]));
        assert_eq!(note.huffman, Some(&[0xAB, 0xCD][..]));
    }

    #[test]
    fn reads_aoc_note_with_file_offsets() {
        // The same IFD, but its offsets count from a file that starts 4 bytes earlier.
        let pentax = note();
        let mut file = b"head".to_vec();
        file.extend(b"AOC\0II");
        let mut ifd = pentax[10..].to_vec();
        let values = 4 + 6 + 2 + 3 * 12 + 4;
        ifd[10..14].copy_from_slice(&(values as u32).to_le_bytes());
        ifd[22..26].copy_from_slice(&(values as u32 + 8).to_le_bytes());
        file.extend(ifd);
        let note = MakerNote::parse(&file, &file[4..]).unwrap();
        assert_eq!(note.white_balance, Some([17600.0, 8192.0, 13248.0]));
        assert_eq!(note.black, Some([64.0, 65.0, 66.0, 67.0]));
    }

    #[test]
    fn ignores_other_notes() {
        let note = MakerNote::parse(b"Nikon\0", b"Nikon\0").unwrap();
        assert!(note.huffman.is_none());
    }
}
