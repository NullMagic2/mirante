//! Module: mirante_raw_fuji::raw_data
//! Purpose: Finds the raw photosite data and its levels inside the RAF raw section.
//! Created: 2026-11-16
//! Architecture: Current cameras put a little TIFF there whose 0xF000 IFD gives
//! size, bit depth, data offset, black levels and white balance (as dcraw's
//! parse_tiff_ifd reads them, by Dave Coffin). Older ones store bare 16-bit
//! big-endian samples sized by the CFA header's 0x100 record.

use crate::header::{RafHeader, Size};
use mirante_codec::Endian;
use mirante_codec::tiff::{Ifd, Tiff};
use mirante_core::DecodeError;
use mirante_raw::BlackLevel;

const RAW_IFD: u16 = 0xF000;
const WIDTH: u16 = 0xF001;
const HEIGHT: u16 = 0xF002;
const BITS: u16 = 0xF003;
const DATA_OFFSET: u16 = 0xF007;
const DATA_LENGTH: u16 = 0xF008;
const BLACK_LEVELS: u16 = 0xF00A;
/// As-shot white balance as G, R, B; 0xF00E is the one dcraw reads.
const WHITE_BALANCE: [u16; 2] = [0xF00E, 0xF00D];
const LEGACY_BITS: u32 = 14;
const MAX_BITS: u32 = 16;

/// The coded photosites, before decompression or unpacking.
#[derive(Debug)]
pub struct RawData<'a> {
    pub size: Size,
    pub bits: u32,
    pub bytes: &'a [u8],
    pub endian: Endian,
    pub black: Option<BlackLevel>,
    pub white_balance: Option<[f64; 3]>,
}

pub fn locate<'a>(header: &RafHeader<'a>) -> Result<RawData<'a>, DecodeError> {
    let section = header.raw_section;
    if !Tiff::is_tiff(section) {
        let size = header
            .raw_size
            .ok_or_else(|| DecodeError::corrupt("RAF CFA header lacks the raw size"))?;
        return Ok(RawData {
            size,
            bits: LEGACY_BITS,
            bytes: section,
            endian: Endian::Big,
            black: None,
            white_balance: None,
        });
    }
    let tiff = Tiff::parse(section)?;
    let first = tiff.ifds.first().ok_or_else(|| DecodeError::corrupt("RAF raw IFD is empty"))?;
    let ifd = tiff.child_ifds(first, RAW_IFD)?.into_iter().next().unwrap_or_else(|| first.clone());
    let required = |tag: u16| {
        tiff.get_u32(&ifd, tag)?
            .ok_or_else(|| DecodeError::corrupt(format!("RAF raw IFD lacks tag {tag:#X}")))
    };
    let (offset, length) = (required(DATA_OFFSET)?, required(DATA_LENGTH)?);
    let bytes = section
        .get(offset as usize..)
        .and_then(|rest| rest.get(..length as usize))
        .ok_or_else(|| DecodeError::corrupt("RAF raw data lies outside the file"))?;
    let bits = required(BITS)?;
    if !(1..=MAX_BITS).contains(&bits) {
        return Err(DecodeError::unsupported(format!("RAF with {bits}-bit samples")));
    }
    Ok(RawData {
        size: Size { height: required(HEIGHT)? as usize, width: required(WIDTH)? as usize },
        bits,
        bytes,
        endian: tiff.endian(),
        black: black_level(&tiff, &ifd)?,
        white_balance: white_balance(&tiff, &ifd)?,
    })
}

/// A square repeat block of black levels from the plane origin, or one value.
fn black_level(tiff: &Tiff<'_>, ifd: &Ifd) -> Result<Option<BlackLevel>, DecodeError> {
    let Some(entry) = ifd.get(BLACK_LEVELS) else {
        return Ok(None);
    };
    let values: Vec<f32> = tiff.unsigned(entry)?.into_iter().map(|v| v as f32).collect();
    let Some(&first) = values.first() else {
        return Err(DecodeError::corrupt("RAF black level tag is empty"));
    };
    if values.iter().all(|&v| v == first) {
        return Ok(Some(BlackLevel::uniform(first)));
    }
    let side = values.len().isqrt();
    if side * side != values.len() {
        return Err(DecodeError::unsupported("RAF black levels that are not a square block"));
    }
    Ok(Some(BlackLevel { width: side, height: side, values }))
}

fn white_balance(tiff: &Tiff<'_>, ifd: &Ifd) -> Result<Option<[f64; 3]>, DecodeError> {
    for tag in WHITE_BALANCE {
        if let Some(entry) = ifd.get(tag) {
            if let [green, red, blue, ..] = tiff.numbers(entry)?[..] {
                return Ok(Some([red, green, blue]));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A little-endian TIFF whose IFD0 points (type IFD) at the given entries.
    pub fn raw_tiff(entries: &[(u16, Vec<u32>)], data: &[u8]) -> Vec<u8> {
        let mut file = b"II*\0\x08\0\0\0".to_vec();
        file.extend(1_u16.to_le_bytes());
        file.extend([0x00, 0xF0, 13, 0, 1, 0, 0, 0]);
        file.extend(26_u32.to_le_bytes());
        file.extend(0_u32.to_le_bytes());
        let values_at = 26 + 2 + 12 * entries.len() + 4;
        let mut values = Vec::new();
        file.extend((entries.len() as u16).to_le_bytes());
        for (tag, list) in entries {
            file.extend(tag.to_le_bytes());
            file.extend(4_u16.to_le_bytes());
            file.extend((list.len() as u32).to_le_bytes());
            if list.len() == 1 {
                file.extend(list[0].to_le_bytes());
            } else {
                file.extend(((values_at + values.len()) as u32).to_le_bytes());
                values.extend(list.iter().flat_map(|v| v.to_le_bytes()));
            }
        }
        file.extend(0_u32.to_le_bytes());
        file.extend(values);
        file.extend(data);
        file
    }

    fn header(section: &[u8]) -> RafHeader<'_> {
        RafHeader { raw_section: section, ..RafHeader::default() }
    }

    #[test]
    fn reads_the_raw_ifd() {
        let data = [1, 2, 3, 4, 5, 6, 7, 8];
        let entries = |offset: usize| {
            [
                (WIDTH, vec![2]),
                (HEIGHT, vec![2]),
                (BITS, vec![14]),
                (DATA_OFFSET, vec![offset as u32]),
                (DATA_LENGTH, vec![8]),
                (BLACK_LEVELS, vec![1022; 36]),
                (0xF00E, vec![302, 588, 563]),
            ]
        };
        let section = raw_tiff(&entries(raw_tiff(&entries(0), &[]).len()), &data);
        let raw = locate(&header(&section)).unwrap();
        assert_eq!(raw.size, Size { height: 2, width: 2 });
        assert_eq!((raw.bits, raw.bytes, raw.endian), (14, data.as_slice(), Endian::Little));
        assert_eq!(raw.black, Some(BlackLevel::uniform(1022.0)));
        assert_eq!(raw.white_balance, Some([588.0, 302.0, 563.0]));
    }

    #[test]
    fn keeps_varying_black_blocks_and_rejects_odd_ones() {
        let block = |values: Vec<u32>| {
            let section = raw_tiff(&[(BLACK_LEVELS, values)], &[]);
            let tiff = Tiff::parse(&section).unwrap();
            let ifd = tiff.child_ifds(&tiff.ifds[0], RAW_IFD).unwrap().remove(0);
            black_level(&tiff, &ifd)
        };
        let level = block(vec![1, 2, 3, 4]).unwrap().unwrap();
        assert_eq!((level.width, level.at(1, 1)), (2, 4.0));
        assert!(block(vec![1, 2, 3]).is_err());
    }

    #[test]
    fn legacy_sections_are_big_endian_and_sized_by_the_header() {
        let mut raf = header(&[0, 1, 0, 2]);
        assert!(locate(&raf).is_err());
        raf.raw_size = Some(Size { height: 1, width: 2 });
        let raw = locate(&raf).unwrap();
        assert_eq!((raw.endian, raw.bits, raw.black), (Endian::Big, LEGACY_BITS, None));
    }

    #[test]
    fn missing_data_is_corrupt() {
        let section = raw_tiff(&[(DATA_OFFSET, vec![4000]), (DATA_LENGTH, vec![8])], &[]);
        assert!(locate(&header(&section)).is_err());
    }
}
