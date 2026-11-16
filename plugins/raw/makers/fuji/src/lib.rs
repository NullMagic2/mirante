//! Module: mirante_raw_fuji
//! Purpose: Decodes Fujifilm RAW files (RAF) into undeveloped photosites.
//! Created: 2026-11-16
//! Architecture: header.rs reads the container, raw_data.rs finds the photosites,
//! and uncompressed.rs or compressed/ decodes them into a NativeRaw (see
//! mirante_raw::frame); the RAW plugin develops it and falls back to the
//! embedded preview when this fails. Ported in part from dcraw by Dave Coffin.

mod compressed;
mod header;
mod raw_data;
mod uncompressed;

use header::{RafHeader, Size, XTRANS_SIDE};
use mirante_core::DecodeError;
use mirante_raw::demosaic::{BLUE, CfaPattern, GREEN, RED};
use mirante_raw::unpack::RawPlane;
use mirante_raw::{Area, NativeRaw};
use raw_data::RawData;

const MAKE: &str = "Fujifilm";
const NO_ORIENTATION: u16 = 1;

pub fn decode(bytes: &[u8]) -> Result<NativeRaw, DecodeError> {
    let header = header::parse(bytes)?;
    if header.super_ccd {
        return Err(DecodeError::unsupported("Fujifilm SuperCCD (45-degree) sensor layout"));
    }
    let raw = raw_data::locate(&header)?;
    let pattern = match header.xtrans {
        Some(colors) => CfaPattern::new(XTRANS_SIDE, XTRANS_SIDE, colors.to_vec())?,
        // dcraw's default for RAF; its margins are even, so the phase holds at the origin.
        None => CfaPattern::new(2, 2, vec![RED, GREEN, GREEN, BLUE])?,
    };
    let (size, bits, samples) = photosites(&raw, &header, &pattern)?;
    let black =
        raw.black.clone().ok_or_else(|| DecodeError::unsupported("RAF without black levels"))?;
    Ok(NativeRaw {
        visible: visible_area(&header, size)?,
        plane: RawPlane {
            width: size.width,
            height: size.height,
            samples_per_pixel: 1,
            bits_per_sample: bits,
            samples,
        },
        pattern: Some(pattern),
        black,
        white: ((1_u32 << bits) - 1) as f32,
        white_balance: raw.white_balance.or(header.white_balance),
        make: MAKE.to_owned(),
        model: header.model.clone(),
        orientation: header::preview_orientation(header.preview).unwrap_or(NO_ORIENTATION),
    })
}

/// Decodes the samples; the layout follows from the data length, as in dcraw.
fn photosites(
    raw: &RawData<'_>,
    header: &RafHeader<'_>,
    pattern: &CfaPattern,
) -> Result<(Size, u32, Vec<u16>), DecodeError> {
    let size = raw.size;
    let pixels = DecodeError::check_size(
        u32::try_from(size.width).unwrap_or(u32::MAX),
        u32::try_from(size.height).unwrap_or(u32::MAX),
    )?;
    let length = raw.bytes.len();
    if length >= pixels * 2 {
        return Ok((size, raw.bits, uncompressed::unpacked(raw.bytes, size, raw.endian)?));
    }
    if length >= uncompressed::packed_row_bytes(size, raw.bits) * size.height {
        return Ok((size, raw.bits, uncompressed::packed(raw.bytes, size, raw.bits)?));
    }
    let coded = compressed::Header::parse(raw.bytes)?;
    if coded.xtrans != header.xtrans.is_some() {
        return Err(DecodeError::corrupt("RAF compression and CFA layout disagree"));
    }
    let samples = compressed::decode(raw.bytes, &coded, |x, y| pattern.color_at(x, y))?;
    Ok((coded.size, coded.bits, samples))
}

/// The camera's own crop; without one, dcraw's centred legacy size or the whole plane.
fn visible_area(header: &RafHeader<'_>, plane: Size) -> Result<Area, DecodeError> {
    let area = match (header.crop_origin, header.crop_size, header.legacy_size) {
        (Some(origin), Some(size), _) => {
            Area { left: origin.width, top: origin.height, width: size.width, height: size.height }
        }
        (_, _, Some(size)) => Area {
            left: plane.width.saturating_sub(size.width) >> 2 << 1,
            top: plane.height.saturating_sub(size.height) >> 2 << 1,
            width: size.width.min(plane.width),
            height: size.height.min(plane.height),
        },
        _ => Area { left: 0, top: 0, width: plane.width, height: plane.height },
    };
    let inside = area.width > 0
        && area.height > 0
        && area.left + area.width <= plane.width
        && area.top + area.height <= plane.height;
    if !inside {
        return Err(DecodeError::corrupt("RAF crop lies outside the raw image"));
    }
    Ok(area)
}

#[cfg(test)]
mod tests {
    use super::*;
    use header::tests::{pair, raf};
    use raw_data::tests::raw_tiff;

    const XTRANS: [u8; 36] = [
        1, 1, 0, 1, 1, 2, 1, 1, 2, 1, 1, 0, 2, 0, 1, 0, 2, 1, //
        1, 1, 2, 1, 1, 0, 1, 1, 0, 1, 1, 2, 0, 2, 1, 2, 0, 1,
    ];

    /// A 6x6 X-Trans RAF with 16-bit little-endian samples 0..36.
    fn uncompressed_raf(data: &[u8]) -> Vec<u8> {
        let entries = |offset: usize| {
            [
                (0xF001, vec![6]),
                (0xF002, vec![6]),
                (0xF003, vec![14]),
                (0xF007, vec![offset as u32]),
                (0xF008, vec![data.len() as u32]),
                (0xF00A, vec![1022; 36]),
                (0xF00E, vec![302, 588, 563]),
            ]
        };
        let section = raw_tiff(&entries(raw_tiff(&entries(0), &[]).len()), data);
        let stored: Vec<u8> = XTRANS.iter().rev().copied().collect();
        let records = [
            (0x100, pair(6, 6)),
            (0x110, pair(1, 2)),
            (0x111, pair(4, 3)),
            (0x130, vec![0x0C, 0x0C, 0x0C, 0x0C]),
            (0x131, stored),
        ];
        raf("X-T3", &records, &section)
    }

    #[test]
    fn decodes_an_uncompressed_raf() {
        let data: Vec<u8> = (0..36_u16).flat_map(|v| v.to_le_bytes()).collect();
        let raw = decode(&uncompressed_raf(&data)).unwrap();
        assert_eq!(raw.plane.samples, (0..36).collect::<Vec<u16>>());
        assert_eq!((raw.plane.width, raw.plane.bits_per_sample), (6, 14));
        assert_eq!(raw.visible, Area { left: 2, top: 1, width: 3, height: 4 });
        let pattern = raw.pattern.unwrap();
        assert_eq!(
            (pattern.color_at(2, 0), pattern.color_at(0, 2), pattern.color_at(6, 7)),
            (0, 2, 1)
        );
        assert_eq!((raw.black.at(3, 3), raw.white), (1022.0, 16383.0));
        assert_eq!(raw.white_balance, Some([588.0, 302.0, 563.0]));
        assert_eq!(
            (raw.make.as_str(), raw.model.as_str(), raw.orientation),
            ("Fujifilm", "X-T3", 1)
        );
    }

    #[test]
    fn damaged_files_fail_without_panicking() {
        let data: Vec<u8> = (0..36_u16).flat_map(|v| v.to_le_bytes()).collect();
        let file = uncompressed_raf(&data);
        for length in 0..file.len() - 1 {
            assert!(decode(&file[..length]).is_err(), "length {length}");
        }
        let mut flipped = file.clone();
        for index in 0..flipped.len() {
            flipped[index] ^= 0x5A;
            let _ = decode(&flipped);
            flipped[index] ^= 0x5A;
        }
    }

    #[test]
    fn compressed_data_must_match_the_layout() {
        // Too short to be unpacked, so it is read as a compressed header.
        let mut data = compressed::tests::header_bytes(true, false, 768, 6);
        data.resize(40, 0);
        let error = decode(&uncompressed_raf(&data)).unwrap_err();
        assert!(error.to_string().contains("disagree"), "{error}");
    }
}
