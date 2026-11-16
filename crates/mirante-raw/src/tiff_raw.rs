//! Module: mirante_raw::tiff_raw
//! Purpose: TIFF-container steps shared by the TIFF-based maker decoders:
//! finding the raw IFD, its strip data, uncompressed samples, CFA pattern,
//! maker note and dcraw-style camera names.
//! Created: 2026-11-16
//! Architecture: Shared by the TIFF-based maker crates; maker-specific
//! decompression stays in each crate. Raw IFD choice follows dcraw's apply_tiff.

use crate::demosaic::{BLUE, CfaPattern};
use crate::unpack::PackedBits;
use mirante_codec::Endian;
use mirante_codec::tiff::{Ifd, Tiff, tag};
use mirante_core::DecodeError;

pub const PHOTOMETRIC_CFA: u32 = 32803;
pub const UNCOMPRESSED: u32 = 1;
const TIFF_EP_CFA_REPEAT_PATTERN_DIM: u16 = 0x828D;
const TIFF_EP_CFA_PATTERN: u16 = 0x828E;
const EXIF_CFA_PATTERN: u16 = 0xA302;
const MAKER_NOTE: u16 = 0x927C;
const MAX_CFA_SIDE: usize = 8;

/// The raw image's directory and the facts every decoder needs from it.
pub struct RawIfd {
    pub ifd: Ifd,
    pub width: usize,
    pub height: usize,
    pub bits: u32,
    pub compression: u32,
}

/// The largest CFA image in IFD0's chain or their SubIFDs whose compression
/// is in `compressions`.
pub fn find_raw_ifd(tiff: &Tiff<'_>, compressions: &[u32]) -> Result<RawIfd, DecodeError> {
    let mut best: Option<(u64, RawIfd)> = None;
    for top in &tiff.ifds {
        for ifd in std::iter::once(top.clone()).chain(tiff.child_ifds(top, tag::SUB_IFDS)?) {
            let value = |tag| tiff.get_u32(&ifd, tag);
            let (Some(width), Some(height)) = (value(tag::IMAGE_WIDTH)?, value(tag::IMAGE_LENGTH)?)
            else {
                continue;
            };
            let bits = value(tag::BITS_PER_SAMPLE)?.unwrap_or(1);
            let compression = value(tag::COMPRESSION)?.unwrap_or(UNCOMPRESSED);
            let is_cfa = value(tag::PHOTOMETRIC)? == Some(PHOTOMETRIC_CFA);
            let size = u64::from(width) * u64::from(height) * u64::from(bits);
            if !is_cfa || size == 0 || !compressions.contains(&compression) {
                continue;
            }
            if best.as_ref().is_none_or(|(largest, _)| size > *largest) {
                let (width, height) = (width as usize, height as usize);
                best = Some((size, RawIfd { ifd, width, height, bits, compression }));
            }
        }
    }
    best.map(|(_, raw)| raw).ok_or_else(|| DecodeError::unsupported("no supported raw image"))
}

/// The raw strips as one slice; they must lie back to back, as cameras write them.
pub fn strip_data<'a>(tiff: &Tiff<'a>, ifd: &Ifd) -> Result<&'a [u8], DecodeError> {
    let list = |tag| ifd.get(tag).map(|entry| tiff.unsigned(entry)).transpose();
    let (Some(offsets), Some(counts)) = (list(tag::STRIP_OFFSETS)?, list(tag::STRIP_BYTE_COUNTS)?)
    else {
        return Err(DecodeError::corrupt("raw image has no strips"));
    };
    let (Some(&start), true) = (offsets.first(), offsets.len() == counts.len()) else {
        return Err(DecodeError::corrupt("raw strip offsets and counts differ"));
    };
    let mut end = u64::from(start);
    for (&offset, &count) in offsets.iter().zip(&counts) {
        if u64::from(offset) != end {
            return Err(DecodeError::unsupported("raw strips that are not contiguous"));
        }
        end += u64::from(count);
    }
    usize::try_from(end)
        .ok()
        .and_then(|end| tiff.data().get(start as usize..end))
        .ok_or_else(|| DecodeError::corrupt("raw data lies outside the file"))
}

/// Uncompressed samples: 16-bit words in the file's byte order when there is
/// room for them, otherwise MSB-first packed `bits`-bit samples.
pub fn unpack_uncompressed(
    data: &[u8],
    raw: &RawIfd,
    endian: Endian,
) -> Result<Vec<u16>, DecodeError> {
    let pixels = DecodeError::check_size(raw.width as u32, raw.height as u32)?;
    if !(8..=16).contains(&raw.bits) {
        return Err(DecodeError::unsupported(format!("{}-bit uncompressed RAW", raw.bits)));
    }
    if data.len() / 2 >= pixels {
        let word = |pair: &[u8]| match endian {
            Endian::Little => u16::from_le_bytes([pair[0], pair[1]]),
            Endian::Big => u16::from_be_bytes([pair[0], pair[1]]),
        };
        return Ok(data.chunks_exact(2).take(pixels).map(word).collect());
    }
    if (data.len() as u64) * 8 < pixels as u64 * u64::from(raw.bits) {
        return Err(DecodeError::corrupt("uncompressed RAW data is truncated"));
    }
    Ok(PackedBits::new(data, raw.bits).take(pixels).collect())
}

/// CFA colours relative to the raw image's origin, from the raw IFD's TIFF/EP
/// tags or else the EXIF CFAPattern.
pub fn cfa_pattern(
    tiff: &Tiff<'_>,
    raw: &Ifd,
    exif: Option<&Ifd>,
) -> Result<CfaPattern, DecodeError> {
    if let (Some(dims), Some(colors)) =
        (raw.get(TIFF_EP_CFA_REPEAT_PATTERN_DIM), raw.get(TIFF_EP_CFA_PATTERN))
    {
        let dims = tiff.unsigned(dims)?;
        let colors = tiff.raw_bytes(colors)?;
        if let [rows, columns] = dims[..] {
            return pattern(columns as usize, rows as usize, colors);
        }
    }
    let entry = exif
        .and_then(|exif| exif.get(EXIF_CFA_PATTERN))
        .ok_or_else(|| DecodeError::unsupported("RAW without a CFA pattern"))?;
    // Columns and rows as two shorts; cameras disagree on their byte order.
    let bytes = tiff.raw_bytes(entry)?;
    let (Some(header), Some(colors)) = (bytes.get(..4), bytes.get(4..)) else {
        return Err(DecodeError::corrupt("EXIF CFAPattern is too short"));
    };
    let little = (usize::from(header[0]), usize::from(header[2]));
    let big = (usize::from(header[1]), usize::from(header[3]));
    let fits = |(columns, rows): (usize, usize)| columns * rows == colors.len();
    let (columns, rows) = if fits(big) && header[0] == 0 { big } else { little };
    pattern(columns, rows, colors)
}

fn pattern(columns: usize, rows: usize, colors: &[u8]) -> Result<CfaPattern, DecodeError> {
    if columns > MAX_CFA_SIDE || rows > MAX_CFA_SIDE || colors.iter().any(|&c| c > BLUE) {
        return Err(DecodeError::unsupported("CFA pattern other than RGB up to 8x8"));
    }
    let colors = colors.get(..columns * rows).ok_or_else(|| DecodeError::corrupt("CFA pattern"))?;
    CfaPattern::new(columns, rows, colors.to_vec())
}

/// IFD0's EXIF directory, if any.
pub fn exif_ifd(tiff: &Tiff<'_>) -> Result<Option<Ifd>, DecodeError> {
    match tiff.ifds.first() {
        Some(ifd0) => Ok(tiff.child_ifds(ifd0, tag::EXIF_IFD)?.into_iter().next()),
        None => Ok(None),
    }
}

/// The EXIF MakerNote bytes.
pub fn maker_note<'a>(
    tiff: &Tiff<'a>,
    exif: Option<&Ifd>,
) -> Result<Option<&'a [u8]>, DecodeError> {
    exif.and_then(|exif| exif.get(MAKER_NOTE)).map(|entry| tiff.raw_bytes(entry)).transpose()
}

/// IFD0's Model with the maker's name and trailing spaces removed, as dcraw
/// names cameras ("NIKON D750" becomes "D750").
pub fn model_name(tiff: &Tiff<'_>, maker: &str) -> Result<String, DecodeError> {
    let model = match tiff.ifds.first().and_then(|ifd| ifd.get(tag::MODEL)) {
        Some(entry) => tiff.ascii(entry)?,
        None => return Err(DecodeError::corrupt("RAW file has no camera model")),
    };
    Ok(strip_maker(&model, maker).to_owned())
}

fn strip_maker<'a>(model: &'a str, maker: &str) -> &'a str {
    let prefixed = model.len() > maker.len()
        && model.is_char_boundary(maker.len())
        && model[..maker.len()].eq_ignore_ascii_case(maker)
        && model[maker.len()..].starts_with(' ');
    let model = if prefixed { &model[maker.len() + 1..] } else { model };
    model.trim()
}

/// An ASCII field of `ifd`, empty when absent.
pub fn text(tiff: &Tiff<'_>, ifd: &Ifd, field: u16) -> Result<String, DecodeError> {
    ifd.get(field).map_or(Ok(String::new()), |entry| tiff.ascii(entry))
}

/// IFD0's orientation, 1 when absent or out of range.
pub fn orientation(tiff: &Tiff<'_>) -> Result<u16, DecodeError> {
    let value = match tiff.ifds.first() {
        Some(ifd0) => tiff.get_u32(ifd0, tag::ORIENTATION)?.unwrap_or(1),
        None => 1,
    };
    Ok(if (1..=8).contains(&value) { value as u16 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demosaic::{GREEN, RED};

    #[test]
    fn strips_maker_from_model() {
        assert_eq!(strip_maker("NIKON Z 6", "Nikon"), "Z 6");
        assert_eq!(strip_maker("PENTAX K-1        ", "Pentax"), "K-1");
        assert_eq!(strip_maker("COOLPIX P7000", "Nikon"), "COOLPIX P7000");
    }

    #[test]
    fn builds_rggb_pattern() {
        let pattern = pattern(2, 2, &[0, 1, 1, 2]).unwrap();
        assert_eq!(
            [pattern.color_at(0, 0), pattern.color_at(1, 0), pattern.color_at(1, 1)],
            [RED, GREEN, BLUE]
        );
        assert!(super::pattern(2, 2, &[0, 1, 1, 3]).is_err());
    }
}
