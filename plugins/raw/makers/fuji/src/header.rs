//! Module: mirante_raw_fuji::header
//! Purpose: Reads the RAF container header and its CFA header records.
//! Created: 2026-11-16
//! Architecture: The big-endian header points at the preview JPEG, the CFA
//! header (tag/length records: sizes, crop, X-Trans layout, white balance) and
//! the raw data section that raw_data.rs reads. Record meanings follow dcraw's
//! parse_fuji, by Dave Coffin.

use mirante_codec::tiff::{Tiff, tag};
use mirante_codec::{ByteReader, Endian};
use mirante_core::DecodeError;

pub const MAGIC: &[u8] = b"FUJIFILMCCD-RAW ";
const MODEL_AT: usize = 28;
const MODEL_LENGTH: usize = 32;
const PREVIEW_AT: usize = 84;
const CFA_HEADER_AT: usize = 92;
const CFA_AT: usize = 100;
/// dcraw gives up on CFA headers with more records than this.
const MAX_RECORDS: u32 = 255;
const RAW_SIZE: u16 = 0x100;
const CROP_ORIGIN: u16 = 0x110;
const CROP_SIZE: u16 = 0x111;
const LEGACY_SIZE: u16 = 0x121;
const LAYOUT: u16 = 0x130;
const XTRANS_LAYOUT: u16 = 0x131;
const WHITE_BALANCE: u16 = 0x2FF0;
pub const XTRANS_SIDE: usize = 6;
/// Layout byte 1 without this bit marks a 45-degree SuperCCD sensor.
const LAYOUT_STRAIGHT: u8 = 8;
/// The preview's APP1 holds a TIFF after the marker, length and "Exif\0\0".
const EXIF_TIFF_AT: usize = 12;

/// Height and width, in the order the records store them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub height: usize,
    pub width: usize,
}

/// What the RAF header and CFA header records say.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RafHeader<'a> {
    pub model: String,
    pub preview: Option<&'a [u8]>,
    /// The raw data section; TIFF offsets inside it are relative to its start.
    pub raw_section: &'a [u8],
    pub raw_size: Option<Size>,
    /// The camera's own crop: (top, left) and size.
    pub crop_origin: Option<Size>,
    pub crop_size: Option<Size>,
    pub legacy_size: Option<Size>,
    pub super_ccd: bool,
    /// X-Trans colours (0 red, 1 green, 2 blue), row-major from the raw origin.
    pub xtrans: Option<[u8; XTRANS_SIDE * XTRANS_SIDE]>,
    /// Red, green, blue multipliers.
    pub white_balance: Option<[f64; 3]>,
}

fn slice(data: &[u8], offset: u32, length: u32) -> Option<&[u8]> {
    data.get(offset as usize..)?.get(..length as usize)
}

pub fn parse(data: &[u8]) -> Result<RafHeader<'_>, DecodeError> {
    if !data.starts_with(MAGIC) {
        return Err(DecodeError::corrupt("not a Fujifilm RAF file"));
    }
    let mut reader = ByteReader::new(data, Endian::Big).at(PREVIEW_AT)?;
    let (preview_offset, preview_length) = (reader.u32()?, reader.u32()?);
    let mut reader = reader.at(CFA_HEADER_AT)?;
    let (records_offset, records_length) = (reader.u32()?, reader.u32()?);
    let mut reader = reader.at(CFA_AT)?;
    let (raw_offset, raw_length) = (reader.u32()?, reader.u32()?);

    let model_field = data.get(MODEL_AT..MODEL_AT + MODEL_LENGTH).unwrap_or_default();
    let model = model_field.split(|&b| b == 0).next().unwrap_or_default();
    let model = String::from_utf8_lossy(model).trim().to_owned();
    let raw_section = slice(data, raw_offset, raw_length)
        .ok_or_else(|| DecodeError::corrupt("RAF raw data lies outside the file"))?;
    let records = slice(data, records_offset, records_length)
        .ok_or_else(|| DecodeError::corrupt("RAF CFA header lies outside the file"))?;
    let mut header = RafHeader {
        model: model.strip_prefix("FinePix ").unwrap_or(&model).to_owned(),
        preview: slice(data, preview_offset, preview_length),
        raw_section,
        ..RafHeader::default()
    };
    read_records(records, &mut header)?;
    Ok(header)
}

fn read_records(records: &[u8], header: &mut RafHeader<'_>) -> Result<(), DecodeError> {
    let mut reader = ByteReader::new(records, Endian::Big);
    let count = reader.u32()?;
    if count > MAX_RECORDS {
        return Err(DecodeError::corrupt("RAF CFA header has too many records"));
    }
    for _ in 0..count {
        let record_tag = reader.u16()?;
        let length = usize::from(reader.u16()?);
        let payload = reader.bytes(length)?;
        let mut value = ByteReader::new(payload, Endian::Big);
        let mut size = || -> Result<Size, DecodeError> {
            Ok(Size { height: usize::from(value.u16()?), width: usize::from(value.u16()?) })
        };
        match record_tag {
            RAW_SIZE => header.raw_size = Some(size()?),
            CROP_ORIGIN => header.crop_origin = Some(size()?),
            CROP_SIZE => header.crop_size = Some(size()?),
            LEGACY_SIZE => header.legacy_size = Some(size()?),
            LAYOUT => {
                let layout = payload.get(..2).ok_or_else(short_record)?;
                header.super_ccd = layout[0] >> 7 != 0 || layout[1] & LAYOUT_STRAIGHT == 0;
            }
            XTRANS_LAYOUT => header.xtrans = Some(xtrans_layout(payload)?),
            WHITE_BALANCE => {
                // Stored as G, R, G, B.
                let mut levels = [0.0; 4];
                for level in &mut levels {
                    *level = f64::from(value.u16()?);
                }
                header.white_balance = Some([levels[1], levels[0], levels[3]]);
            }
            _ => {}
        }
    }
    Ok(())
}

fn short_record() -> DecodeError {
    DecodeError::corrupt("RAF CFA header record is too short")
}

/// The record lists the 6x6 colours last-to-first.
fn xtrans_layout(payload: &[u8]) -> Result<[u8; XTRANS_SIDE * XTRANS_SIDE], DecodeError> {
    let stored = payload.get(..XTRANS_SIDE * XTRANS_SIDE).ok_or_else(short_record)?;
    let mut colors = [0; XTRANS_SIDE * XTRANS_SIDE];
    for (target, &source) in colors.iter_mut().zip(stored.iter().rev()) {
        *target = source & 3;
    }
    Ok(colors)
}

/// EXIF orientation of the preview JPEG; RAF keeps none for the raw itself.
pub fn preview_orientation(preview: Option<&[u8]>) -> Option<u16> {
    let exif = preview?;
    if exif.get(..4)? != [0xFF, 0xD8, 0xFF, 0xE1] {
        return None;
    }
    let tiff = Tiff::parse(exif.get(EXIF_TIFF_AT..)?).ok()?;
    let orientation = tiff.get_u32(tiff.ifds.first()?, tag::ORIENTATION).ok()??;
    u16::try_from(orientation).ok().filter(|o| (1..=8).contains(o))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A RAF with the given CFA header records and raw section; no preview.
    pub fn raf(model: &str, records: &[(u16, Vec<u8>)], raw: &[u8]) -> Vec<u8> {
        let mut table = (records.len() as u32).to_be_bytes().to_vec();
        for (record_tag, payload) in records {
            table.extend(record_tag.to_be_bytes());
            table.extend((payload.len() as u16).to_be_bytes());
            table.extend(payload);
        }
        let mut file = MAGIC.to_vec();
        file.extend(b"0201");
        file.resize(MODEL_AT, 0);
        file.extend(model.as_bytes());
        file.resize(120, 0);
        let records_at = file.len() as u32;
        let raw_at = records_at + table.len() as u32;
        let fields = [0, 0, records_at, table.len() as u32, raw_at, raw.len() as u32];
        for (index, field) in fields.iter().enumerate() {
            let at = PREVIEW_AT + 4 * index;
            file[at..at + 4].copy_from_slice(&field.to_be_bytes());
        }
        file.extend(table);
        file.extend(raw);
        file
    }

    pub fn pair(first: u16, second: u16) -> Vec<u8> {
        [first.to_be_bytes(), second.to_be_bytes()].concat()
    }

    #[test]
    fn reads_records() {
        let layout: Vec<u8> = (0..36).map(|i| (i % 3) as u8).collect();
        let records = [
            (RAW_SIZE, pair(4038, 6048)),
            (CROP_ORIGIN, pair(21, 16)),
            (CROP_SIZE, pair(4000, 6000)),
            (LAYOUT, vec![0x0C, 0x0C, 0x0C, 0x0C]),
            (XTRANS_LAYOUT, layout),
            (WHITE_BALANCE, [pair(302, 588), pair(302, 563)].concat()),
            (0x9650, vec![1, 2, 3, 4]),
        ];
        let file = raf("FinePix X100", &records, &[7, 8]);
        let header = parse(&file).unwrap();
        assert_eq!(header.model, "X100");
        assert_eq!(header.raw_section, [7, 8]);
        assert_eq!(header.raw_size, Some(Size { height: 4038, width: 6048 }));
        assert_eq!(header.crop_origin, Some(Size { height: 21, width: 16 }));
        assert_eq!(header.crop_size, Some(Size { height: 4000, width: 6000 }));
        assert!(!header.super_ccd);
        let xtrans = header.xtrans.unwrap();
        assert_eq!((xtrans[0], xtrans[1], xtrans[35]), (35 % 3, 34 % 3, 0));
        assert_eq!(header.white_balance, Some([588.0, 302.0, 563.0]));
    }

    #[test]
    fn rejects_bad_headers() {
        assert!(parse(b"FUJIFILM").is_err());
        let mut file = raf("X-T3", &[], &[]);
        file[CFA_AT..CFA_AT + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(parse(&file).is_err());
        let file = raf("X-T3", &[(XTRANS_LAYOUT, vec![0; 35])], &[]);
        assert!(parse(&file).is_err());
    }

    #[test]
    fn reads_preview_orientation() {
        let tiff =
            [b"MM\0*\0\0\0\x08\0\x01".as_slice(), &[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0]]
                .concat();
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE1, 0, 0];
        jpeg.extend(b"Exif\0\0");
        jpeg.extend(&tiff);
        jpeg.extend([0, 0, 0, 0]);
        assert_eq!(preview_orientation(Some(&jpeg)), Some(6));
        assert_eq!(preview_orientation(Some(&[0xFF, 0xD8, 0xFF, 0xDB])), None);
    }
}
