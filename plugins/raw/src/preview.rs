//! Module: mirante_format_raw::preview
//! Purpose: Shows the largest JPEG preview embedded in RAW files we cannot develop.
//! Created: 2026-11-16
//! Architecture: Collects JPEG streams from TIFF IFDs, the RAF header or CR3 boxes,
//! ranks them by their SOF size and decodes the best one with the JPEG plugin.
//! That decoder applies the JPEG's own EXIF orientation, so the RAW file's
//! orientation is applied only to previews without one.

use mirante_codec::isobmff::{self, IsoBox};
use mirante_codec::tiff::{Ifd, Tiff, tag};
use mirante_core::{ColorSpace, DecodeError, Image};

/// Panasonic RW2 stores a full-size JPEG in IFD0 under this tag.
const RW2_JPEG_FROM_RAW: u16 = 0x002E;
const OLD_JPEG: u32 = 6;
const JPEG: u32 = 7;
const RAF_PREVIEW_OFFSET_AT: usize = 84;
/// Canon's metadata box inside `moov`, and the box holding the PRVW preview.
const CANON_METADATA_UUID: [u8; 16] = [
    0x85, 0xC0, 0xB6, 0x87, 0x82, 0x0F, 0x11, 0xE0, 0x81, 0x11, 0xF4, 0xCE, 0x46, 0x2B, 0x6A, 0x48,
];
const CANON_PREVIEW_UUID: [u8; 16] = [
    0xEA, 0xF4, 0x2B, 0x5E, 0x1C, 0x98, 0x4B, 0x88, 0xB9, 0xFB, 0xB7, 0xDC, 0x40, 0x6E, 0x4D, 0x16,
];
/// The preview uuid payload starts with 8 bytes before its PRVW box.
const CANON_PREVIEW_HEADER: usize = 8;
const JPEG_SOI: [u8; 3] = [0xFF, 0xD8, 0xFF];
const EXIF_SIGNATURE: &[u8] = b"Exif\0\0";
const NO_ORIENTATION: u16 = 1;
/// SubIFDs of SubIFDs hold previews in some NEF and DNG files.
const SUB_IFD_DEPTH: usize = 2;

/// Which preview to decode when a file has several.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    /// The largest, for showing instead of the raw data.
    Largest,
    /// The smallest that still has this many pixels, for calibrating colour.
    AtLeast(u64),
}

/// What the JPEG headers say before any pixel is decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct JpegInfo {
    pixels: u64,
    lossless: bool,
    has_orientation: bool,
}

pub fn from_tiff(tiff: &Tiff<'_>, want: Want) -> Result<Image, DecodeError> {
    let mut ifds = tiff.ifds.clone();
    let mut level = tiff.ifds.clone();
    for _ in 0..SUB_IFD_DEPTH {
        level = level
            .iter()
            .flat_map(|ifd| tiff.child_ifds(ifd, tag::SUB_IFDS).unwrap_or_default())
            .collect();
        ifds.extend(level.iter().cloned());
    }
    let candidates = ifds.iter().flat_map(|ifd| ifd_jpegs(tiff, ifd)).collect();
    let orientation = tiff
        .ifds
        .first()
        .and_then(|ifd| tiff.get_u32(ifd, tag::ORIENTATION).ok().flatten())
        .unwrap_or(1);
    decode_best(candidates, orientation as u16, want)
}

/// JPEG streams an IFD points at: JPEGInterchangeFormat, a JPEG strip, or RW2's tag.
fn ifd_jpegs<'a>(tiff: &Tiff<'a>, ifd: &Ifd) -> Vec<&'a [u8]> {
    let value = |tag| tiff.get_u32(ifd, tag).ok().flatten();
    let slice = |offset: u32, length: u32| {
        tiff.data().get(offset as usize..).and_then(|rest| rest.get(..length as usize))
    };
    let mut found = Vec::new();

    if let (Some(offset), Some(length)) =
        (value(tag::JPEG_INTERCHANGE_FORMAT), value(tag::JPEG_INTERCHANGE_FORMAT_LENGTH))
    {
        found.extend(slice(offset, length));
    }
    if matches!(value(tag::COMPRESSION), Some(OLD_JPEG | JPEG)) {
        let list = |tag| ifd.get(tag).and_then(|entry| tiff.unsigned(entry).ok());
        if let (Some([offset]), Some([length])) =
            (list(tag::STRIP_OFFSETS).as_deref(), list(tag::STRIP_BYTE_COUNTS).as_deref())
        {
            found.extend(slice(*offset, *length));
        }
    }
    if let Some(entry) = ifd.get(RW2_JPEG_FROM_RAW) {
        found.extend(tiff.raw_bytes(entry).ok());
    }
    found
}

/// Fujifilm RAF: a big-endian offset and length of the preview at byte 84.
pub fn from_raf(data: &[u8], want: Want) -> Result<Image, DecodeError> {
    decode_best(raf_preview(data).into_iter().collect(), NO_ORIENTATION, want)
}

fn raf_preview(data: &[u8]) -> Option<&[u8]> {
    let field = |at: usize| {
        data.get(at..at + 4).map(|b| u32::from_be_bytes(b.try_into().expect("four bytes")) as usize)
    };
    let (offset, length) = (field(RAF_PREVIEW_OFFSET_AT)?, field(RAF_PREVIEW_OFFSET_AT + 4)?);
    data.get(offset..)?.get(..length)
}

/// Canon CR3: the full-size JPEG of the first track, and the PRVW preview.
pub fn from_cr3(data: &[u8], want: Want) -> Result<Image, DecodeError> {
    let moov = isobmff::boxes(data).require(*b"moov")?;
    let mut candidates = Vec::new();
    candidates.extend(first_track_sample(data, &moov)?);
    for item in isobmff::boxes(data) {
        let item = item?;
        if item.user_type == Some(CANON_PREVIEW_UUID) {
            candidates.extend(canon_preview(&item));
        }
    }
    decode_best(candidates, cr3_orientation(&moov).unwrap_or(NO_ORIENTATION), want)
}

/// The first sample of the first `trak`, located through its sample table.
fn first_track_sample<'a>(
    data: &'a [u8],
    moov: &IsoBox<'a>,
) -> Result<Option<&'a [u8]>, DecodeError> {
    let Some(track) = moov.children().find(*b"trak")? else {
        return Ok(None);
    };
    let mut table = track;
    for name in [b"mdia", b"minf", b"stbl"] {
        table = table.children().require(*name)?;
    }
    let mut sizes = table.children().require(*b"stsz")?.full()?.reader();
    let (fixed_size, count) = (sizes.u32()?, sizes.u32()?);
    let size = if fixed_size != 0 || count == 0 { fixed_size } else { sizes.u32()? };
    let offset = match table.children().find(*b"co64")? {
        Some(co64) => {
            let mut reader = co64.full()?.reader();
            reader.skip(4)?;
            reader.u64()?
        }
        None => {
            let mut reader = table.children().require(*b"stco")?.full()?.reader();
            reader.skip(4)?;
            u64::from(reader.u32()?)
        }
    };
    let start = usize::try_from(offset).ok();
    Ok(start.and_then(|start| data.get(start..)?.get(..size as usize)))
}

/// The JPEG inside the PRVW box, which follows a short dimensions header.
fn canon_preview<'a>(item: &IsoBox<'a>) -> Option<&'a [u8]> {
    let boxes = item.payload.get(CANON_PREVIEW_HEADER..)?;
    let child = isobmff::boxes(boxes).find(*b"PRVW").ok()??;
    let start = child.payload.windows(JPEG_SOI.len()).position(|w| w == JPEG_SOI)?;
    Some(&child.payload[start..])
}

/// Orientation from CMT1, the TIFF IFD0 Canon keeps in its metadata uuid box.
fn cr3_orientation(moov: &IsoBox<'_>) -> Option<u16> {
    let canon = moov.children().flatten().find(|b| b.user_type == Some(CANON_METADATA_UUID))?;
    let cmt1 = canon.children().find(*b"CMT1").ok()??;
    let tiff = Tiff::parse(cmt1.payload).ok()?;
    let orientation = tiff.get_u32(tiff.ifds.first()?, tag::ORIENTATION).ok()??;
    Some(orientation as u16)
}

/// Decodes the best decodable baseline JPEG among the candidates for `want`.
fn decode_best(candidates: Vec<&[u8]>, orientation: u16, want: Want) -> Result<Image, DecodeError> {
    let mut ranked: Vec<(JpegInfo, &[u8])> = candidates
        .into_iter()
        .filter_map(|jpeg| Some((jpeg_info(jpeg)?, jpeg)))
        .filter(|(info, _)| !info.lossless)
        .collect();
    ranked.sort_by_key(|(info, _)| std::cmp::Reverse(info.pixels));
    if let Want::AtLeast(minimum) = want {
        // Big enough ones first, smallest of those first; the rest largest first.
        ranked.sort_by_key(|(info, _)| info.pixels < minimum);
        let split =
            ranked.iter().position(|(info, _)| info.pixels < minimum).unwrap_or(ranked.len());
        ranked[..split].reverse();
    }
    let mut last_error = None;

    for (info, jpeg) in ranked {
        match mirante_format_jpeg::decode(jpeg) {
            Ok(image) => {
                let image = if info.has_orientation { image } else { image.oriented(orientation) };
                return Ok(match image.color_space() {
                    ColorSpace::Icc(_) => image,
                    _ => image.with_color_space(ColorSpace::Srgb),
                });
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error
        .unwrap_or_else(|| DecodeError::unsupported("RAW file without an embedded preview")))
}

/// Walks JPEG markers up to the frame header; None if this is not a JPEG.
fn jpeg_info(jpeg: &[u8]) -> Option<JpegInfo> {
    if !jpeg.starts_with(&JPEG_SOI) {
        return None;
    }
    let mut position = 2;
    let mut has_orientation = None;

    loop {
        let marker = *jpeg.get(position + 1)?;
        if jpeg[position] != 0xFF {
            return None;
        }
        if marker == 0xFF {
            position += 1;
            continue;
        }
        let length =
            usize::from(u16::from_be_bytes([*jpeg.get(position + 2)?, *jpeg.get(position + 3)?]));
        let payload = jpeg.get(position + 4..position + 2 + length.max(2))?;
        match marker {
            0xE1 if has_orientation.is_none() => {
                if let Some(exif) = payload.strip_prefix(EXIF_SIGNATURE) {
                    has_orientation = Some(exif_has_orientation(exif));
                }
            }
            0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                let height = u64::from(u16::from_be_bytes([*payload.get(1)?, *payload.get(2)?]));
                let width = u64::from(u16::from_be_bytes([*payload.get(3)?, *payload.get(4)?]));
                return Some(JpegInfo {
                    pixels: width * height,
                    lossless: matches!(marker, 0xC3 | 0xC7 | 0xCB | 0xCF),
                    has_orientation: has_orientation.unwrap_or(false),
                });
            }
            0xDA => return None,
            _ => {}
        }
        position += 2 + length;
    }
}

fn exif_has_orientation(exif: &[u8]) -> bool {
    Tiff::parse(exif)
        .is_ok_and(|tiff| tiff.ifds.first().is_some_and(|ifd| ifd.get(tag::ORIENTATION).is_some()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(marker: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0xFF, marker];
        out.extend(((payload.len() + 2) as u16).to_be_bytes());
        out.extend(payload);
        out
    }

    fn jpeg_header(sof: u8, width: u16, height: u16, exif: Option<&[u8]>) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8];
        if let Some(exif) = exif {
            out.extend(segment(0xE1, &[EXIF_SIGNATURE, exif].concat()));
        }
        let mut frame = vec![8];
        frame.extend(height.to_be_bytes());
        frame.extend(width.to_be_bytes());
        frame.extend([1, 1, 0x11, 0]);
        out.extend(segment(sof, &frame));
        out
    }

    #[test]
    fn reads_frame_size_and_kind() {
        let info = jpeg_info(&jpeg_header(0xC0, 640, 480, None)).unwrap();
        assert_eq!(info, JpegInfo { pixels: 640 * 480, lossless: false, has_orientation: false });
        assert!(jpeg_info(&jpeg_header(0xC3, 64, 64, None)).unwrap().lossless);
        assert!(jpeg_info(b"not a jpeg").is_none());
    }

    #[test]
    fn notices_exif_orientation() {
        let exif =
            crate::testing::tiff(vec![(tag::ORIENTATION, crate::testing::Value::Short(vec![6]))]);
        let info = jpeg_info(&jpeg_header(0xC2, 8, 8, Some(&exif))).unwrap();
        assert!(info.has_orientation);
    }

    #[test]
    fn missing_preview_is_unsupported() {
        let file =
            crate::testing::tiff(vec![(tag::IMAGE_WIDTH, crate::testing::Value::Short(vec![8]))]);
        let error = from_tiff(&Tiff::parse(&file).unwrap(), Want::Largest).unwrap_err();
        assert!(matches!(error, DecodeError::Unsupported(_)));
    }

    #[test]
    fn finds_jpegs_in_ifds_raf_and_cr3() {
        use crate::testing::Value;
        let big = jpeg_header(0xC0, 600, 400, None);
        let small = jpeg_header(0xC0, 160, 120, None);
        let file = crate::testing::tiff(vec![
            (tag::COMPRESSION, Value::Short(vec![6])),
            (tag::STRIP_OFFSETS, Value::DataOffset(big.clone())),
            (tag::STRIP_BYTE_COUNTS, Value::Long(vec![big.len() as u32])),
            (
                tag::SUB_IFDS,
                Value::SubIfds(vec![vec![(RW2_JPEG_FROM_RAW, Value::Undefined(small.clone()))]]),
            ),
        ]);
        let tiff = Tiff::parse(&file).unwrap();
        let found: Vec<_> = std::iter::once(&tiff.ifds[0])
            .chain(&tiff.child_ifds(&tiff.ifds[0], tag::SUB_IFDS).unwrap())
            .flat_map(|ifd| ifd_jpegs(&tiff, ifd))
            .collect();
        assert_eq!(found, [big.as_slice(), small.as_slice()]);

        let mut raf = b"FUJIFILMCCD-RAW 0201".to_vec();
        raf.resize(92, 0);
        raf[84..88].copy_from_slice(&92_u32.to_be_bytes());
        raf[88..92].copy_from_slice(&(small.len() as u32).to_be_bytes());
        raf.extend(&small);
        assert_eq!(raf_preview(&raf), Some(small.as_slice()));
    }

    #[test]
    fn locates_cr3_first_track_sample() {
        use crate::testing::iso_box as b;
        let jpeg = jpeg_header(0xC0, 6000, 4000, None);
        let full = |payload: &[u8]| [&[0, 0, 0, 0][..], payload].concat();
        let mut stsz = 0_u32.to_be_bytes().to_vec();
        stsz.extend(1_u32.to_be_bytes());
        stsz.extend((jpeg.len() as u32).to_be_bytes());
        let ftyp = b(b"ftyp", b"crx \0\0\0\x01crx isom");
        let build = |offset: u32| {
            let mut stco = 1_u32.to_be_bytes().to_vec();
            stco.extend(offset.to_be_bytes());
            let stbl = [b(b"stsz", &full(&stsz)), b(b"stco", &full(&stco))].concat();
            let trak = b(b"trak", &b(b"mdia", &b(b"minf", &b(b"stbl", &stbl))));
            [ftyp.clone(), b(b"moov", &trak)].concat()
        };
        let header = build(0).len() as u32;
        let mut file = build(header);
        file.extend(&jpeg);
        let moov = isobmff::boxes(&file).require(*b"moov").unwrap();
        assert_eq!(first_track_sample(&file, &moov).unwrap(), Some(jpeg.as_slice()));
    }

    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let pixels = vec![[0.5, 0.5, 0.5, 1.0]; (width * height) as usize];
        let image = Image::new(width, height, pixels, ColorSpace::Srgb).unwrap();
        mirante_format_jpeg::encode(&image).unwrap()
    }

    #[test]
    fn picks_the_preview_size_asked_for() {
        let (thumb, small, large) = (jpeg(16, 8), jpeg(64, 32), jpeg(128, 64));
        let candidates = || vec![&thumb[..], &large[..], &small[..]];
        let width = |want| decode_best(candidates(), 1, want).unwrap().width();
        assert_eq!(width(Want::Largest), 128);
        assert_eq!(width(Want::AtLeast(1000)), 64);
        assert_eq!(width(Want::AtLeast(1_000_000)), 128);
    }
}
