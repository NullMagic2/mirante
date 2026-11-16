//! Module: mirante_raw_olympus::maker_note
//! Purpose: Reads white balance, black level and crop from the Olympus maker note.
//! Created: 2026-11-16
//! Architecture: Follows dcraw's parse_makernote by Dave Coffin. "OLYMPUS\0" notes
//! carry their own byte order and offsets relative to the note; older "OLYMP\0"
//! notes use the file's. Levels live in the ImageProcessing sub-IFD (0x2040).

use mirante_codec::Endian;
use mirante_codec::tiff::{Entry, Ifd, Tiff, tag};
use mirante_core::DecodeError;
use mirante_raw::Area;

const MAKER_NOTE: u16 = 0x927c;
const IMAGE_PROCESSING: u16 = 0x2040;
const FIELD_UNDEFINED: u16 = 7;

mod note_tag {
    pub const BLACK_LEVEL: u16 = 0x1012;
    pub const RED_BALANCE: u16 = 0x1017;
    pub const BLUE_BALANCE: u16 = 0x1018;
}

/// Tags of the ImageProcessing sub-IFD.
mod processing_tag {
    pub const WB_RB_LEVELS: u16 = 0x0100;
    pub const BLACK_LEVEL_2: u16 = 0x0600;
    pub const CROP_LEFT: u16 = 0x0612;
    pub const CROP_TOP: u16 = 0x0613;
    pub const CROP_WIDTH: u16 = 0x0614;
    pub const CROP_HEIGHT: u16 = 0x0615;
}

/// White balance gains are fixed point with 8 fractional bits.
const GAIN_ONE: f64 = 256.0;

#[derive(Debug, Default, PartialEq)]
pub struct MakerNote {
    /// Red and blue gains relative to green.
    pub red_blue: Option<[f64; 2]>,
    /// Red, green on red rows, green on blue rows, blue.
    pub black: Option<[f32; 4]>,
    pub crop: Option<Area>,
}

impl MakerNote {
    /// Finds the note through IFD0's EXIF IFD; an absent note gives defaults.
    pub fn read(tiff: &Tiff<'_>) -> Result<Self, DecodeError> {
        let Some(main) = tiff.ifds.first() else {
            return Ok(Self::default());
        };
        for exif in tiff.child_ifds(main, tag::EXIF_IFD)? {
            if let Some(entry) = exif.get(MAKER_NOTE) {
                let note = tiff.raw_bytes(entry)?;
                let start = offset_in(tiff.data(), note);
                return Self::parse(tiff, start, note.len());
            }
        }
        Ok(Self::default())
    }

    /// Parses the note at `start` in the file.
    fn parse(tiff: &Tiff<'_>, start: usize, length: usize) -> Result<Self, DecodeError> {
        let note = &tiff.data()[start..start + length];
        let (reader, ifd_offset) = if note.starts_with(b"OLYMPUS\0") {
            let endian = match note.get(8..10) {
                Some(b"II") => Endian::Little,
                Some(b"MM") => Endian::Big,
                _ => return Err(DecodeError::corrupt("Olympus maker note byte order")),
            };
            (Tiff::headerless(&tiff.data()[start..], endian), 12)
        } else if note.starts_with(b"OLYMP\0") {
            (Tiff::headerless(tiff.data(), tiff.endian()), start + 8)
        } else {
            return Ok(Self::default());
        };
        let (ifd, _) = reader.read_ifd(ifd_offset)?;
        let mut result = Self::default();
        result.read_main(&reader, &ifd)?;
        if let Some(entry) = ifd.get(IMAGE_PROCESSING) {
            let offset = sub_ifd_offset(&reader, entry)?;
            let (processing, _) = reader.read_ifd(offset)?;
            result.read_processing(&reader, &processing)?;
        }
        Ok(result)
    }

    /// Older cameras keep levels in the note itself.
    fn read_main(&mut self, tiff: &Tiff<'_>, ifd: &Ifd) -> Result<(), DecodeError> {
        let red = first(tiff, ifd, note_tag::RED_BALANCE)?;
        let blue = first(tiff, ifd, note_tag::BLUE_BALANCE)?;
        if let (Some(red), Some(blue)) = (red, blue) {
            self.red_blue = gains(red, blue);
        }
        if let Some(black) = four(tiff, ifd, note_tag::BLACK_LEVEL)? {
            self.black = Some(black);
        }
        Ok(())
    }

    fn read_processing(&mut self, tiff: &Tiff<'_>, ifd: &Ifd) -> Result<(), DecodeError> {
        if let Some(entry) = ifd.get(processing_tag::WB_RB_LEVELS) {
            if let [red, blue, ..] = tiff.numbers(entry)?[..] {
                self.red_blue = gains(red, blue);
            }
        }
        if let Some(black) = four(tiff, ifd, processing_tag::BLACK_LEVEL_2)? {
            self.black = Some(black);
        }
        let crop = [
            processing_tag::CROP_LEFT,
            processing_tag::CROP_TOP,
            processing_tag::CROP_WIDTH,
            processing_tag::CROP_HEIGHT,
        ]
        .map(|field| first(tiff, ifd, field));
        if let [Ok(Some(left)), Ok(Some(top)), Ok(Some(width)), Ok(Some(height))] = crop {
            let [left, top, width, height] = [left, top, width, height].map(|v| v as usize);
            self.crop = Some(Area { left, top, width, height });
        }
        Ok(())
    }
}

/// The sub-IFD pointer is a LONG/IFD value or, in old notes, the position of an
/// UNDEFINED blob that holds the directory.
fn sub_ifd_offset(tiff: &Tiff<'_>, entry: &Entry) -> Result<usize, DecodeError> {
    if entry.field_type == FIELD_UNDEFINED {
        return Ok(offset_in(tiff.data(), tiff.raw_bytes(entry)?));
    }
    let offsets = tiff.unsigned(entry)?;
    let offset = offsets.first().ok_or_else(|| DecodeError::corrupt("empty Olympus sub-IFD"))?;
    Ok(*offset as usize)
}

/// Position of a subslice within the slice it was borrowed from.
fn offset_in(data: &[u8], part: &[u8]) -> usize {
    part.as_ptr().addr() - data.as_ptr().addr()
}

fn gains(red: f64, blue: f64) -> Option<[f64; 2]> {
    (red > 0.0 && blue > 0.0).then(|| [red / GAIN_ONE, blue / GAIN_ONE])
}

fn first(tiff: &Tiff<'_>, ifd: &Ifd, field: u16) -> Result<Option<f64>, DecodeError> {
    match ifd.get(field) {
        Some(entry) => Ok(tiff.numbers(entry)?.first().copied()),
        None => Ok(None),
    }
}

fn four(tiff: &Tiff<'_>, ifd: &Ifd, field: u16) -> Result<Option<[f32; 4]>, DecodeError> {
    let Some(entry) = ifd.get(field) else {
        return Ok(None);
    };
    Ok(tiff.numbers(entry)?.get(..4).map(|v| [v[0], v[1], v[2], v[3]].map(|b| b as f32)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(tag: u16, field_type: u16, count: u32, value: u32) -> Vec<u8> {
        let mut bytes = tag.to_le_bytes().to_vec();
        bytes.extend(field_type.to_le_bytes());
        bytes.extend(count.to_le_bytes());
        bytes.extend(value.to_le_bytes());
        bytes
    }

    /// A little-endian TIFF whose EXIF IFD holds an "OLYMPUS\0II" maker note
    /// with an ImageProcessing sub-IFD.
    fn sample() -> Vec<u8> {
        const NOTE: usize = 44;
        let mut note = b"OLYMPUS\0II\x03\0".to_vec();
        note.extend(1_u16.to_le_bytes());
        note.extend(entry(IMAGE_PROCESSING, 13, 1, 30));
        note.extend(0_u32.to_le_bytes());
        // ImageProcessing at note offset 30, values from note offset 30 + 2 + 7 * 12 + 4.
        note.extend(7_u16.to_le_bytes());
        note.extend(entry(0x0100, 3, 2, 500 | 470 << 16));
        note.extend(entry(0x0600, 3, 4, 120));
        note.extend(entry(0x0612, 3, 2, 8));
        note.extend(entry(0x0613, 3, 2, 6));
        note.extend(entry(0x0614, 4, 1, 4608));
        note.extend(entry(0x0615, 4, 1, 3456));
        note.extend(entry(0x0001, 3, 1, 0));
        note.extend(0_u32.to_le_bytes());
        assert_eq!(note.len(), 120);
        for black in [254_u16, 255, 256, 257] {
            note.extend(black.to_le_bytes());
        }
        let mut file = b"II*\0".to_vec();
        file.extend(8_u32.to_le_bytes());
        file.extend(1_u16.to_le_bytes());
        file.extend(entry(tag::EXIF_IFD, 4, 1, 26));
        file.extend(0_u32.to_le_bytes());
        file.extend(1_u16.to_le_bytes());
        file.extend(entry(MAKER_NOTE, 7, note.len() as u32, NOTE as u32));
        file.extend(0_u32.to_le_bytes());
        assert_eq!(file.len(), NOTE);
        file.extend(note);
        file
    }

    #[test]
    fn reads_image_processing_levels_and_crop() {
        let data = sample();
        let tiff = Tiff::parse(&data).unwrap();
        let note = MakerNote::read(&tiff).unwrap();
        assert_eq!(note.red_blue, Some([500.0 / 256.0, 470.0 / 256.0]));
        assert_eq!(note.black, Some([254.0, 255.0, 256.0, 257.0]));
        assert_eq!(note.crop, Some(Area { left: 8, top: 6, width: 4608, height: 3456 }));
    }

    #[test]
    fn ignores_other_maker_notes() {
        let mut data = sample();
        data[44..51].copy_from_slice(b"NIKON\0\0");
        let tiff = Tiff::parse(&data).unwrap();
        assert_eq!(MakerNote::read(&tiff).unwrap(), MakerNote::default());
    }
}
