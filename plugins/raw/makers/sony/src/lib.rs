//! Module: mirante_raw_sony
//! Purpose: Decodes Sony RAW files (ARW) into undeveloped photosites.
//! Created: 2026-11-16
//! Architecture: ARW is TIFF with the raw image in a SubIFD of IFD0. arw2.rs
//! unpacks compressed data; sr2.rs decrypts the private levels. Ported from dcraw
//! by Dave Coffin, with LibRaw's 14-bit scale. Returns a NativeRaw (see
//! mirante_raw::frame); the RAW plugin develops it.

mod arw2;
mod sr2;

use mirante_codec::Endian;
use mirante_codec::tiff::{Ifd, Tiff, tag};
use mirante_core::DecodeError;
use mirante_raw::demosaic::CfaPattern;
use mirante_raw::unpack::RawPlane;
use mirante_raw::{Area, BlackLevel, NativeRaw, tiff_raw};

use arw2::ToneCurve;
use sr2::Sr2;

const UNCOMPRESSED: u32 = 1;
const LOSSLESS_JPEG: u32 = 7;
const SONY_COMPRESSED: u32 = 32767;
const PHOTOMETRIC_CFA: u32 = 32803;
/// Both formats are delivered on a 14-bit scale.
const OUTPUT_BITS: u32 = 14;

mod sony_tag {
    pub const CFA_REPEAT_PATTERN_DIM: u16 = 0x828d;
    pub const CFA_PATTERN: u16 = 0x828e;
    pub const TONE_CURVE: u16 = 0x7010;
    pub const BLACK_RGBG: u16 = 0x7300;
    pub const WB_GRBG: u16 = 0x7303;
    pub const BLACK_RGGB: u16 = 0x7310;
    pub const WB_RGGB: u16 = 0x7313;
    pub const WHITE_LEVEL: u16 = 0xc61d;
    pub const DEFAULT_CROP_ORIGIN: u16 = 0xc61f;
    pub const DEFAULT_CROP_SIZE: u16 = 0xc620;
}

pub fn decode(bytes: &[u8]) -> Result<NativeRaw, DecodeError> {
    let tiff = Tiff::parse(bytes)?;
    let main = tiff.ifds.first().ok_or_else(|| DecodeError::corrupt("ARW has no IFD"))?;
    let raw = raw_ifd(&tiff)?;
    let plane = read_plane(&tiff, &raw)?;

    let mut levels = Levels::default();
    levels.read(&tiff, &raw)?;
    if let Some(sr2) = Sr2::find(&tiff)? {
        let (sub_tiff, sub_ifd) = sr2.ifd()?;
        levels.read(&sub_tiff, &sub_ifd)?;
    }
    let pattern = cfa_pattern(&tiff, &raw)?;
    let default_black = if plane.width > 3888 { 512.0 } else { 0.0 };
    let model = tiff_raw::text(&tiff, main, tag::MODEL)?;
    Ok(NativeRaw {
        visible: visible_area(&tiff, &raw, &plane, &model)?,
        black: BlackLevel::bayer(&pattern, levels.black.unwrap_or([default_black; 4])),
        white: levels.white.unwrap_or(((1 << OUTPUT_BITS) - 1) as f32),
        white_balance: levels.white_balance,
        pattern: Some(pattern),
        plane,
        make: "Sony".to_owned(),
        model,
        orientation: tiff_raw::orientation(&tiff)?,
    })
}

/// The largest CFA image among IFD0's chain and their SubIFDs.
fn raw_ifd(tiff: &Tiff<'_>) -> Result<Ifd, DecodeError> {
    let mut candidates = Vec::new();
    for ifd in &tiff.ifds {
        candidates.push(ifd.clone());
        candidates.extend(tiff.child_ifds(ifd, tag::SUB_IFDS)?);
    }
    let mut best = None;
    let mut best_area = 0;
    for ifd in candidates {
        if tiff.get_u32(&ifd, tag::PHOTOMETRIC)? != Some(PHOTOMETRIC_CFA) {
            continue;
        }
        let width = tiff.get_u32(&ifd, tag::IMAGE_WIDTH)?.unwrap_or(0);
        let height = tiff.get_u32(&ifd, tag::IMAGE_LENGTH)?.unwrap_or(0);
        let area = u64::from(width) * u64::from(height);
        if area > best_area {
            (best, best_area) = (Some(ifd), area);
        }
    }
    best.ok_or_else(|| DecodeError::unsupported("ARW without a CFA raw image"))
}

fn read_plane(tiff: &Tiff<'_>, raw: &Ifd) -> Result<RawPlane, DecodeError> {
    let width = tiff.get_u32(raw, tag::IMAGE_WIDTH)?.unwrap_or(0);
    let height = tiff.get_u32(raw, tag::IMAGE_LENGTH)?.unwrap_or(0);
    let pixels = DecodeError::check_size(width, height)?;
    let (width, height) = (width as usize, height as usize);
    let compression = tiff.get_u32(raw, tag::COMPRESSION)?.unwrap_or(UNCOMPRESSED);
    let offset = tiff.get_u32(raw, tag::STRIP_OFFSETS)?;
    let count = tiff.get_u32(raw, tag::STRIP_BYTE_COUNTS)?;
    let (Some(offset), Some(count)) = (offset, count) else {
        return Err(DecodeError::unsupported("ARW raw image without a single strip"));
    };
    let data = tiff
        .data()
        .get(offset as usize..)
        .and_then(|rest| rest.get(..count as usize))
        .ok_or_else(|| DecodeError::corrupt("ARW raw data lies outside the file"))?;
    let samples = match compression {
        SONY_COMPRESSED if data.len() == pixels => {
            let curve = match raw.get(sony_tag::TONE_CURVE) {
                Some(entry) => ToneCurve::from_knees(&tiff.unsigned(entry)?),
                None => ToneCurve::identity(),
            };
            arw2::decode(data, width, height, &curve)?
        }
        SONY_COMPRESSED => return Err(DecodeError::unsupported("Sony ARW 1 compression")),
        UNCOMPRESSED if data.len() == pixels * 2 => words(data, tiff.endian()),
        UNCOMPRESSED => return Err(DecodeError::unsupported("packed uncompressed ARW")),
        LOSSLESS_JPEG => return Err(DecodeError::unsupported("lossless-compressed ARW")),
        other => return Err(DecodeError::unsupported(format!("ARW compression {other}"))),
    };
    Ok(RawPlane { width, height, samples_per_pixel: 1, bits_per_sample: OUTPUT_BITS, samples })
}

fn words(data: &[u8], endian: Endian) -> Vec<u16> {
    data.chunks_exact(2)
        .map(|pair| match endian {
            Endian::Little => u16::from_le_bytes([pair[0], pair[1]]),
            Endian::Big => u16::from_be_bytes([pair[0], pair[1]]),
        })
        .collect()
}

/// Levels and white balance; later directories override earlier ones, as in dcraw.
#[derive(Default)]
struct Levels {
    /// Red, green on red rows, green on blue rows, blue.
    black: Option<[f32; 4]>,
    white: Option<f32>,
    white_balance: Option<[f64; 3]>,
}

impl Levels {
    fn read(&mut self, tiff: &Tiff<'_>, ifd: &Ifd) -> Result<(), DecodeError> {
        let four = |tag| -> Result<Option<[f64; 4]>, DecodeError> {
            let Some(entry) = ifd.get(tag) else {
                return Ok(None);
            };
            Ok(tiff.numbers(entry)?.get(..4).map(|v| [v[0], v[1], v[2], v[3]]))
        };
        if let Some(v) = four(sony_tag::BLACK_RGBG)? {
            self.black = Some([v[0], v[1], v[3], v[2]].map(|b| b as f32));
        }
        if let Some(v) = four(sony_tag::BLACK_RGGB)? {
            self.black = Some(v.map(|b| b as f32));
        }
        if let Some(v) = four(sony_tag::WB_GRBG)? {
            self.white_balance = normalised([v[1], v[0], v[2]]);
        }
        if let Some(v) = four(sony_tag::WB_RGGB)? {
            // dcraw's test for cameras that store the greens first.
            let rgb = if v[1] == 1024.0 && v[2] == 1024.0 {
                [v[0], v[1], v[3]]
            } else {
                [v[1], v[0], v[2]]
            };
            self.white_balance = normalised(rgb);
        }
        // SR2's 0x787f is a lower linearity limit; LibRaw keeps 0xc61d as the white.
        if let Some(white) = tiff.get_u32(ifd, sony_tag::WHITE_LEVEL)?.filter(|&w| w > 0) {
            self.white = Some(white as f32);
        }
        Ok(())
    }
}

/// Multipliers relative to green; None when they are unusable.
fn normalised([red, green, blue]: [f64; 3]) -> Option<[f64; 3]> {
    (red > 0.0 && green > 0.0 && blue > 0.0).then(|| [red / green, 1.0, blue / green])
}

fn cfa_pattern(tiff: &Tiff<'_>, raw: &Ifd) -> Result<CfaPattern, DecodeError> {
    let dimensions = match raw.get(sony_tag::CFA_REPEAT_PATTERN_DIM) {
        Some(entry) => tiff.unsigned(entry)?,
        None => vec![2, 2],
    };
    let colors = match raw.get(sony_tag::CFA_PATTERN) {
        Some(entry) => tiff.unsigned(entry)?.into_iter().map(|c| c.min(255) as u8).collect(),
        None => vec![0, 1, 1, 2],
    };
    match dimensions[..] {
        [rows, columns] => CfaPattern::new(columns as usize, rows as usize, colors),
        _ => Err(DecodeError::corrupt("ARW CFA pattern dimensions")),
    }
}

/// DefaultCrop when present; otherwise dcraw's per-sensor right margins.
fn visible_area(
    tiff: &Tiff<'_>,
    raw: &Ifd,
    plane: &RawPlane,
    model: &str,
) -> Result<Area, DecodeError> {
    let pair = |tag| -> Result<Option<(usize, usize)>, DecodeError> {
        let Some(entry) = raw.get(tag) else {
            return Ok(None);
        };
        Ok(match tiff.numbers(entry)?[..] {
            [x, y] if x >= 0.0 && y >= 0.0 => Some((x as usize, y as usize)),
            _ => None,
        })
    };
    let area = match (pair(sony_tag::DEFAULT_CROP_ORIGIN)?, pair(sony_tag::DEFAULT_CROP_SIZE)?) {
        (Some((left, top)), Some((width, height))) => Area { left, top, width, height },
        _ => Area {
            left: 0,
            top: 0,
            width: plane.width.saturating_sub(right_margin(plane, model)),
            height: plane.height,
        },
    };
    let fits = area.width > 0
        && area.height > 0
        && area.width <= plane.width.saturating_sub(area.left)
        && area.height <= plane.height.saturating_sub(area.top);
    fits.then_some(area).ok_or_else(|| DecodeError::corrupt("ARW crop lies outside the raw image"))
}

/// Columns of masked or invalid data at the right, from dcraw's identify().
fn right_margin(plane: &RawPlane, model: &str) -> usize {
    match plane.width {
        3984 => 59,
        4288 | 8000 => 32,
        4928 if plane.height < 3280 => 8,
        5504 if plane.height > 3664 => 8,
        5504 => 32,
        6048 if model.contains("RX1") || model.contains("A99") => 30,
        6048 => 24,
        7392 => 30,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_balance_is_relative_to_green() {
        assert_eq!(normalised([2048.0, 1024.0, 1536.0]), Some([2.0, 1.0, 1.5]));
        assert_eq!(normalised([0.0, 1024.0, 1536.0]), None);
    }

    /// Decodes a sample named by MIRANTE_SONY_SAMPLE.
    #[test]
    #[ignore]
    fn decodes_sample_from_environment() {
        let path = std::env::var("MIRANTE_SONY_SAMPLE").expect("MIRANTE_SONY_SAMPLE");
        let bytes = std::fs::read(path).unwrap();
        let start = std::time::Instant::now();
        let raw = decode(&bytes).unwrap();
        eprintln!(
            "{} {} {}x{} visible {:?} black {:?} white {} wb {:?} orientation {} in {:?}",
            raw.make,
            raw.model,
            raw.plane.width,
            raw.plane.height,
            raw.visible,
            raw.black,
            raw.white,
            raw.white_balance,
            raw.orientation,
            start.elapsed()
        );
        if let Ok(out) = std::env::var("MIRANTE_RAW_DUMP") {
            let bytes: Vec<u8> = raw.plane.samples.iter().flat_map(|v| v.to_le_bytes()).collect();
            std::fs::write(out, bytes).unwrap();
        }
    }
}
