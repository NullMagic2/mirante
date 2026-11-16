//! Module: mirante_raw_panasonic
//! Purpose: Decodes Panasonic RAW files (RW2) into undeveloped photosites.
//! Created: 2026-11-16
//! Architecture: RW2 is TIFF with magic 0x55 and Panasonic's own tags in IFD0;
//! blocks.rs decodes formats 1 to 4. Tag meanings ported from dcraw by Dave
//! Coffin. Returns a NativeRaw (see mirante_raw::frame); the RAW plugin develops it.

mod blocks;

use mirante_codec::tiff::{Ifd, Tiff, tag};
use mirante_core::DecodeError;
use mirante_raw::demosaic::CfaPattern;
use mirante_raw::unpack::RawPlane;
use mirante_raw::{Area, BlackLevel, NativeRaw, tiff_raw};

/// RW2 blocks are stored rotated at this offset.
const RW2_SPLIT: usize = 0x2008;
/// Newer bit-packed encodings (5 and up) are not block-compressed.
const LAST_BLOCK_FORMAT: u32 = 4;
/// dcraw and LibRaw add this to the black levels the file states.
const BLACK_OFFSET: f32 = 15.0;
const DEFAULT_BITS: u32 = 12;

mod pana_tag {
    pub const SENSOR_WIDTH: u16 = 0x0002;
    pub const SENSOR_HEIGHT: u16 = 0x0003;
    pub const SENSOR_TOP: u16 = 0x0004;
    pub const SENSOR_LEFT: u16 = 0x0005;
    pub const SENSOR_BOTTOM: u16 = 0x0006;
    pub const SENSOR_RIGHT: u16 = 0x0007;
    pub const CFA_PATTERN: u16 = 0x0009;
    pub const BITS_PER_SAMPLE: u16 = 0x000a;
    pub const RED_BALANCE: u16 = 0x0011;
    pub const BLUE_BALANCE: u16 = 0x0012;
    pub const BLACK_RED: u16 = 0x001c;
    pub const BLACK_GREEN: u16 = 0x001d;
    pub const BLACK_BLUE: u16 = 0x001e;
    pub const WB_RED: u16 = 0x0024;
    pub const WB_GREEN: u16 = 0x0025;
    pub const WB_BLUE: u16 = 0x0026;
    pub const RAW_FORMAT: u16 = 0x002d;
    pub const RAW_DATA_OFFSET: u16 = 0x0118;
}

pub fn decode(bytes: &[u8]) -> Result<NativeRaw, DecodeError> {
    let tiff = Tiff::parse(bytes)?;
    let main = tiff.ifds.first().ok_or_else(|| DecodeError::corrupt("RW2 has no IFD"))?;
    let field = |tag| tiff.get_u32(main, tag);
    let bits = field(pana_tag::BITS_PER_SAMPLE)?.unwrap_or(DEFAULT_BITS).clamp(1, 16);
    let plane = read_plane(&tiff, main, bits)?;
    let pattern = cfa_pattern(field(pana_tag::CFA_PATTERN)?.unwrap_or(1))?;
    let black =
        |tag| -> Result<f32, DecodeError> { Ok(field(tag)?.unwrap_or(0) as f32 + BLACK_OFFSET) };
    let red = black(pana_tag::BLACK_RED)?;
    let green = black(pana_tag::BLACK_GREEN)?;
    let blue = black(pana_tag::BLACK_BLUE)?;
    Ok(NativeRaw {
        visible: visible_area(&tiff, main, &plane)?,
        black: BlackLevel::bayer(&pattern, [red, green, green, blue]),
        white: ((1_u32 << bits) - 1) as f32,
        white_balance: white_balance(&tiff, main)?,
        pattern: Some(pattern),
        plane,
        make: make_name(&tiff_raw::text(&tiff, main, tag::MAKE)?),
        model: tiff_raw::text(&tiff, main, tag::MODEL)?,
        orientation: tiff_raw::orientation(&tiff)?,
    })
}

fn read_plane(tiff: &Tiff<'_>, ifd: &Ifd, bits: u32) -> Result<RawPlane, DecodeError> {
    let width = tiff.get_u32(ifd, pana_tag::SENSOR_WIDTH)?.unwrap_or(0);
    let height = tiff.get_u32(ifd, pana_tag::SENSOR_HEIGHT)?.unwrap_or(0);
    DecodeError::check_size(width, height)?;
    let format = tiff.get_u32(ifd, pana_tag::RAW_FORMAT)?.unwrap_or(1);
    if format > LAST_BLOCK_FORMAT {
        return Err(DecodeError::unsupported(format!("Panasonic raw format {format}")));
    }
    let Some(offset) = tiff.get_u32(ifd, pana_tag::RAW_DATA_OFFSET)? else {
        return Err(DecodeError::unsupported("Panasonic RAW without RW2 block data"));
    };
    let data = tiff
        .data()
        .get(offset as usize..)
        .ok_or_else(|| DecodeError::corrupt("RW2 raw data lies outside the file"))?;
    let (width, height) = (width as usize, height as usize);
    let samples = blocks::decode(data, width, height, RW2_SPLIT)?;
    Ok(RawPlane { width, height, samples_per_pixel: 1, bits_per_sample: bits, samples })
}

/// Panasonic numbers the four Bayer phases from 1: RGGB, GRBG, GBRG, BGGR.
fn cfa_pattern(code: u32) -> Result<CfaPattern, DecodeError> {
    let colors = match code {
        1 => [0, 1, 1, 2],
        2 => [1, 0, 2, 1],
        3 => [1, 2, 0, 1],
        4 => [2, 1, 1, 0],
        _ => return Err(DecodeError::unsupported(format!("Panasonic CFA pattern {code}"))),
    };
    CfaPattern::new(2, 2, colors.to_vec())
}

/// The sensor borders, which match the camera's own JPEG size.
fn visible_area(tiff: &Tiff<'_>, ifd: &Ifd, plane: &RawPlane) -> Result<Area, DecodeError> {
    let border = |tag| tiff.get_u32(ifd, tag).map(|value| value.map(|v| v as usize));
    let top = border(pana_tag::SENSOR_TOP)?.unwrap_or(0);
    let left = border(pana_tag::SENSOR_LEFT)?.unwrap_or(0);
    let bottom = border(pana_tag::SENSOR_BOTTOM)?.unwrap_or(plane.height);
    let right = border(pana_tag::SENSOR_RIGHT)?.unwrap_or(plane.width);
    if top >= bottom || left >= right || bottom > plane.height || right > plane.width {
        return Err(DecodeError::corrupt("RW2 sensor borders lie outside the raw image"));
    }
    Ok(Area { left, top, width: right - left, height: bottom - top })
}

/// WBRedLevel/Green/Blue, or the older 8.8 fixed-point red and blue gains.
fn white_balance(tiff: &Tiff<'_>, ifd: &Ifd) -> Result<Option<[f64; 3]>, DecodeError> {
    let field = |tag| tiff.get_u32(ifd, tag).map(|value| value.map(f64::from));
    let levels = [pana_tag::WB_RED, pana_tag::WB_GREEN, pana_tag::WB_BLUE].map(field);
    let (red, green, blue) = match levels {
        [Ok(Some(red)), Ok(Some(green)), Ok(Some(blue))] => (red, green, blue),
        _ => match (field(pana_tag::RED_BALANCE)?, field(pana_tag::BLUE_BALANCE)?) {
            (Some(red), Some(blue)) => (red, 256.0, blue),
            _ => return Ok(None),
        },
    };
    let usable = red > 0.0 && green > 0.0 && blue > 0.0;
    Ok(usable.then(|| [red / green, 1.0, blue / green]))
}

/// The colour table names makers as dcraw does.
fn make_name(make: &str) -> String {
    if make.to_uppercase().starts_with("LEICA") { "Leica".to_owned() } else { make.to_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_panasonic_cfa_codes() {
        let pattern = cfa_pattern(2).unwrap();
        assert_eq!((pattern.color_at(0, 0), pattern.color_at(1, 0)), (1, 0));
        assert!(cfa_pattern(5).is_err());
    }

    /// Decodes a sample named by MIRANTE_PANASONIC_SAMPLE.
    #[test]
    #[ignore]
    fn decodes_sample_from_environment() {
        let path = std::env::var("MIRANTE_PANASONIC_SAMPLE").expect("MIRANTE_PANASONIC_SAMPLE");
        let bytes = std::fs::read(path).unwrap();
        let start = std::time::Instant::now();
        let raw = decode(&bytes).unwrap();
        eprintln!("{:?} {:?} in {:?}", raw.visible, raw.white_balance, start.elapsed());
        eprintln!("{} {} {:?} {} {}", raw.make, raw.model, raw.black, raw.white, raw.orientation);
        if let Ok(out) = std::env::var("MIRANTE_RAW_DUMP") {
            let bytes: Vec<u8> = raw.plane.samples.iter().flat_map(|v| v.to_le_bytes()).collect();
            std::fs::write(out, bytes).unwrap();
        }
    }
}
