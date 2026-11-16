//! Module: mirante_raw_canon::cr3
//! Purpose: Reads CR3 files: finds the full-resolution CRX track and its
//! sample, and the camera metadata in the CMT boxes and the CTMD track.
//! Created: 2026-11-16
//! Architecture: ISO-BMFF through mirante_codec::isobmff; layout from Laurent
//! Clévy's CR3 description. CMT1 is IFD0, CMT3 the maker note; newer bodies
//! keep ColorData in a CTMD record instead. crx/ decodes the sample.

use crate::assemble::{self, Camera, MAKE};
use crate::crx::{self, header::ImageHeader};
use crate::maker_note::MakerNote;
use mirante_codec::isobmff::{self, IsoBox};
use mirante_codec::tiff::Tiff;
use mirante_codec::{ByteReader, Endian};
use mirante_core::DecodeError;
use mirante_raw::NativeRaw;
use mirante_raw::demosaic::{BLUE, CfaPattern, GREEN, RED};
use mirante_raw::tiff_raw;
use mirante_raw::unpack::RawPlane;

const CANON_UUID: [u8; 16] = [
    0x85, 0xC0, 0xB6, 0x87, 0x82, 0x0F, 0x11, 0xE0, 0x81, 0x11, 0xF4, 0xCE, 0x46, 0x2B, 0x6A, 0x48,
];
/// Bytes of the CRAW visual sample entry before its child boxes.
const CRAW_ENTRY_HEADER: usize = 82;
/// CTMD records: size (LE u32), type (LE u16), ...; types 7 to 9 carry a TIFF
/// at this offset, type 8 the maker-note tags (ColorData).
const CTMD_RECORD_HEADER: usize = 6;
const CTMD_TIFF_OFFSET: usize = 20;
const CTMD_MAKER_NOTE: u16 = 8;
const PLANE_COLORS: [u8; 4] = [RED, GREEN, GREEN, BLUE];

/// A track's single sample and its sample entry.
struct Track<'a> {
    entry: IsoBox<'a>,
    sample: &'a [u8],
}

pub fn decode(bytes: &[u8]) -> Result<NativeRaw, DecodeError> {
    let moov = isobmff::boxes(bytes).require(*b"moov")?;
    let canon = moov
        .children()
        .filter_map(Result::ok)
        .find(|item| item.user_type == Some(CANON_UUID))
        .ok_or_else(|| DecodeError::corrupt("CR3 lacks Canon's metadata box"))?;
    let ifd0 = Tiff::parse(canon.children().require(*b"CMT1")?.payload)?;
    let mut note = match canon.children().find(*b"CMT3")? {
        Some(cmt3) => first_ifd_note(cmt3.payload)?,
        None => MakerNote::default(),
    };
    let mut raw: Option<(ImageHeader, &[u8])> = None;
    for trak in moov.children().filter_map(Result::ok).filter(|item| item.box_type == *b"trak") {
        let track = track(bytes, trak)?;
        match &track.entry.box_type {
            b"CRAW" => {
                let children = track.entry.payload.get(CRAW_ENTRY_HEADER..).unwrap_or_default();
                let Some(cmp1) = isobmff::boxes(children).find(*b"CMP1")? else {
                    continue;
                };
                let header = crx::header::parse_cmp1(cmp1.payload)?;
                let larger = raw.as_ref().is_none_or(|(best, _)| {
                    header.width * header.height > best.width * best.height
                });
                if larger {
                    raw = Some((header, track.sample));
                }
            }
            b"CTMD" => note = note.merge(ctmd_maker_note(track.sample)?),
            _ => {}
        }
    }
    let (header, sample) =
        raw.ok_or_else(|| DecodeError::unsupported("CR3 without a CRX raw track"))?;
    let mosaic = crx::decode(&header, sample)?;
    let colors = (0..4)
        .map(|cell| {
            let plane =
                (0..4).find(|&p| crx::plane_offset(p, header.cfa_layout) == (cell & 1, cell >> 1));
            PLANE_COLORS[plane.expect("each cell has one plane")]
        })
        .collect();
    let pattern = CfaPattern::new(2, 2, colors)?;
    let plane = RawPlane {
        width: mosaic.width,
        height: mosaic.height,
        samples_per_pixel: 1,
        bits_per_sample: header.bits,
        samples: mosaic.samples,
    };
    let camera = Camera {
        model: tiff_raw::model_name(&ifd0, MAKE)?,
        orientation: tiff_raw::orientation(&ifd0)?,
        note,
    };
    Ok(assemble::bayer(plane, pattern, camera))
}

/// The first sample entry and first sample of a track.
fn track<'a>(file: &'a [u8], trak: IsoBox<'a>) -> Result<Track<'a>, DecodeError> {
    let stbl = trak
        .children()
        .require(*b"mdia")?
        .children()
        .require(*b"minf")?
        .children()
        .require(*b"stbl")?;
    let stsd = stbl.children().require(*b"stsd")?.full()?;
    let entry = stsd
        .children_after(4)?
        .next()
        .transpose()?
        .ok_or_else(|| DecodeError::corrupt("CR3 track has no sample entry"))?;
    let size = sample_size(stbl.children().require(*b"stsz")?)?;
    let offset = chunk_offset(stbl)?;
    let sample = usize::try_from(offset)
        .ok()
        .and_then(|offset| file.get(offset..)?.get(..size))
        .ok_or_else(|| DecodeError::corrupt("CR3 sample lies outside the file"))?;
    Ok(Track { entry, sample })
}

fn sample_size(stsz: IsoBox<'_>) -> Result<usize, DecodeError> {
    let mut reader = stsz.full()?.reader();
    let fixed = reader.u32()?;
    let count = reader.u32()?;
    let size = if fixed != 0 || count == 0 { fixed } else { reader.u32()? };
    Ok(size as usize)
}

fn chunk_offset(stbl: IsoBox<'_>) -> Result<u64, DecodeError> {
    if let Some(co64) = stbl.children().find(*b"co64")? {
        let mut reader = co64.full()?.reader();
        reader.skip(4)?;
        return reader.u64();
    }
    let mut reader = stbl.children().require(*b"stco")?.full()?.reader();
    reader.skip(4)?;
    Ok(u64::from(reader.u32()?))
}

/// The maker-note tags in the CTMD sample's type 8 record, if any.
fn ctmd_maker_note(sample: &[u8]) -> Result<MakerNote, DecodeError> {
    let mut reader = ByteReader::new(sample, Endian::Little);
    while reader.remaining() >= CTMD_RECORD_HEADER {
        let start = reader.position();
        let size = reader.u32()? as usize;
        let kind = reader.u16()?;
        let record = sample
            .get(start..start.saturating_add(size))
            .filter(|_| size >= CTMD_RECORD_HEADER)
            .ok_or_else(|| DecodeError::corrupt("bad CR3 CTMD record"))?;
        if kind == CTMD_MAKER_NOTE {
            if let Some(tiff) = record.get(CTMD_TIFF_OFFSET..).filter(|data| Tiff::is_tiff(data)) {
                return first_ifd_note(tiff);
            }
        }
        if start + size >= sample.len() {
            break;
        }
        reader = ByteReader::new(sample, Endian::Little).at(start + size)?;
    }
    Ok(MakerNote::default())
}

/// Maker-note tags in the first IFD of an embedded TIFF. Only that IFD is read:
/// Canon's next-IFD links in these blocks are not reliable.
fn first_ifd_note(data: &[u8]) -> Result<MakerNote, DecodeError> {
    let endian = if data.starts_with(b"MM") { Endian::Big } else { Endian::Little };
    let mut reader = ByteReader::new(data, endian);
    reader.skip(4)?;
    let offset = reader.u32()? as usize;
    let tiff = Tiff::headerless(data, endian);
    let (ifd, _) = tiff.read_ifd(offset)?;
    MakerNote::read(&tiff, &ifd)
}
