//! Module: mirante_format_raw::dng
//! Purpose: Develops the raw image of a DNG file into scene-linear Rec. 2020.
//! Created: 2026-11-16
//! Architecture: Finds the raw IFD, unpacks it (unpack.rs) and reads its levels,
//! ActiveArea, DefaultCrop, CFA pattern and colour tags; mirante_raw::develop's
//! shared pipeline then normalises, demosaics, applies white balance and the
//! camera matrix (color.rs) and the Orientation tag. Opcode lists and
//! DefaultScale are ignored.

use mirante_codec::tiff::{Ifd, Tiff, tag};
use mirante_core::{DecodeError, Image};
use mirante_raw::color::{AsShot, Calibration, CameraColor, IDENTITY, Matrix, Rendering};
use mirante_raw::demosaic::{BLUE, CfaPattern};
use mirante_raw::develop::{camera_pixels, finish};
use mirante_raw::levels::{Levels, normalise};
use mirante_raw::unpack::{self, RawPlane};
use mirante_raw::{Area, BlackLevel};

mod dng_tag {
    pub const CFA_REPEAT_PATTERN_DIM: u16 = 33421;
    pub const CFA_PATTERN: u16 = 33422;
    pub const DNG_VERSION: u16 = 50706;
    pub const CFA_PLANE_COLOR: u16 = 50710;
    pub const CFA_LAYOUT: u16 = 50711;
    pub const LINEARIZATION_TABLE: u16 = 50712;
    pub const BLACK_LEVEL_REPEAT_DIM: u16 = 50713;
    pub const BLACK_LEVEL: u16 = 50714;
    pub const BLACK_LEVEL_DELTA_H: u16 = 50715;
    pub const BLACK_LEVEL_DELTA_V: u16 = 50716;
    pub const WHITE_LEVEL: u16 = 50717;
    pub const DEFAULT_CROP_ORIGIN: u16 = 50719;
    pub const DEFAULT_CROP_SIZE: u16 = 50720;
    pub const COLOR_MATRIX: [u16; 2] = [50721, 50722];
    pub const CAMERA_CALIBRATION: [u16; 2] = [50723, 50724];
    pub const ANALOG_BALANCE: u16 = 50727;
    pub const AS_SHOT_NEUTRAL: u16 = 50728;
    pub const AS_SHOT_WHITE_XY: u16 = 50729;
    pub const BASELINE_EXPOSURE: u16 = 50730;
    pub const CALIBRATION_ILLUMINANT: [u16; 2] = [50778, 50779];
    pub const ACTIVE_AREA: u16 = 50829;
    pub const FORWARD_MATRIX: [u16; 2] = [50964, 50965];
}

const PHOTOMETRIC_CFA: u32 = 32803;
const PHOTOMETRIC_LINEAR_RAW: u32 = 34892;
const MAIN_IMAGE: u32 = 0;
const RECTANGULAR_LAYOUT: u32 = 1;
const DEFAULT_PLANE_COLORS: [u8; 3] = [0, 1, 2];
const MAX_LINEARIZATION_ENTRIES: u32 = 1 << 16;
const MAX_BLACK_REPEAT: u32 = 64;

pub fn is_dng(tiff: &Tiff<'_>) -> bool {
    tiff.ifds.first().is_some_and(|ifd| ifd.get(dng_tag::DNG_VERSION).is_some())
}

pub fn decode(tiff: &Tiff<'_>) -> Result<Image, DecodeError> {
    let main = tiff.ifds.first().ok_or_else(|| DecodeError::corrupt("TIFF without an IFD"))?;
    let raw = raw_ifd(tiff)?;
    let photometric = tiff.get_u32(&raw, tag::PHOTOMETRIC)?.unwrap_or_default();
    let plane = unpack::read(tiff, &raw)?;
    let area = active_area(tiff, &raw, &plane)?;
    let levels = levels(tiff, &raw, &plane, area)?;
    let crop = default_crop(tiff, &raw, area)?;
    let spp = plane.samples_per_pixel;
    let pattern = match (photometric, spp) {
        (PHOTOMETRIC_CFA, 1) => Some(cfa_pattern(tiff, &raw)?),
        (PHOTOMETRIC_LINEAR_RAW, 1 | 3) => None,
        _ => {
            return Err(DecodeError::unsupported(format!(
                "DNG photometric {photometric} with {spp} samples"
            )));
        }
    };
    let exposure = tiff_number(tiff, main, dng_tag::BASELINE_EXPOSURE)?.unwrap_or(0.0);
    let rendering = if pattern.is_some() || spp == 3 {
        camera_color(tiff, main, exposure)?.rendering()?
    } else {
        let gain = exposure.exp2() as f32;
        Rendering { matrix: [[gain, 0.0, 0.0], [0.0, gain, 0.0], [0.0, 0.0, gain]], clip: [1.0; 3] }
    };
    let values = normalise(&plane, area, &levels)?;
    let pixels = camera_pixels(values, area, pattern.as_ref(), crop, rendering.gains())?;
    let orientation = tiff.get_u32(main, tag::ORIENTATION)?.unwrap_or(1);
    finish(pixels, crop, &rendering, orientation as u16)
}

/// The full-resolution raw image: NewSubfileType 0 in IFD0 or its SubIFDs.
fn raw_ifd(tiff: &Tiff<'_>) -> Result<Ifd, DecodeError> {
    let main = &tiff.ifds[0];
    let mut candidates = vec![main.clone()];
    candidates.extend(tiff.child_ifds(main, tag::SUB_IFDS)?);
    let mut best: Option<(u64, Ifd)> = None;

    for ifd in candidates {
        let kind = tiff.get_u32(&ifd, tag::NEW_SUBFILE_TYPE)?.unwrap_or(MAIN_IMAGE);
        let photometric = tiff.get_u32(&ifd, tag::PHOTOMETRIC)?;
        let is_raw = matches!(photometric, Some(PHOTOMETRIC_CFA | PHOTOMETRIC_LINEAR_RAW));
        if kind != MAIN_IMAGE || !is_raw {
            continue;
        }
        let width = tiff.get_u32(&ifd, tag::IMAGE_WIDTH)?.unwrap_or(0);
        let height = tiff.get_u32(&ifd, tag::IMAGE_LENGTH)?.unwrap_or(0);
        let area = u64::from(width) * u64::from(height);
        if best.as_ref().is_none_or(|(largest, _)| area > *largest) {
            best = Some((area, ifd));
        }
    }
    best.map(|(_, ifd)| ifd)
        .ok_or_else(|| DecodeError::unsupported("DNG without a CFA or linear raw image"))
}

fn numbers(tiff: &Tiff<'_>, ifd: &Ifd, tag: u16) -> Result<Option<Vec<f64>>, DecodeError> {
    ifd.get(tag).map(|entry| tiff.numbers(entry)).transpose()
}

fn tiff_number(tiff: &Tiff<'_>, ifd: &Ifd, tag: u16) -> Result<Option<f64>, DecodeError> {
    Ok(numbers(tiff, ifd, tag)?.and_then(|values| values.first().copied()))
}

/// ActiveArea (top, left, bottom, right), defaulting to the whole image.
fn active_area(tiff: &Tiff<'_>, ifd: &Ifd, plane: &RawPlane) -> Result<Area, DecodeError> {
    let full = Area { left: 0, top: 0, width: plane.width, height: plane.height };
    let Some(values) = numbers(tiff, ifd, dng_tag::ACTIVE_AREA)? else {
        return Ok(full);
    };
    let [top, left, bottom, right] = values[..] else {
        return Err(DecodeError::corrupt("DNG ActiveArea needs four values"));
    };
    let [top, left, bottom, right] = [top, left, bottom, right].map(|v| v.max(0.0) as usize);
    if bottom > plane.height || right > plane.width || top >= bottom || left >= right {
        return Err(DecodeError::corrupt("DNG ActiveArea lies outside the image"));
    }
    Ok(Area { left, top, width: right - left, height: bottom - top })
}

/// DefaultCropOrigin and DefaultCropSize, relative to the active area.
fn default_crop(tiff: &Tiff<'_>, ifd: &Ifd, area: Area) -> Result<Area, DecodeError> {
    let origin = numbers(tiff, ifd, dng_tag::DEFAULT_CROP_ORIGIN)?;
    let size = numbers(tiff, ifd, dng_tag::DEFAULT_CROP_SIZE)?;
    let (Some([x, y]), Some([width, height])) = (origin.as_deref(), size.as_deref()) else {
        return Ok(Area { left: 0, top: 0, ..area });
    };
    let left = (x.round().max(0.0) as usize).min(area.width - 1);
    let top = (y.round().max(0.0) as usize).min(area.height - 1);
    let width = (width.round().max(1.0) as usize).min(area.width - left);
    let height = (height.round().max(1.0) as usize).min(area.height - top);
    Ok(Area { left, top, width, height })
}

/// BlackLevel (a row x column x sample block aligned to the active area, plus
/// per-row and per-column deltas), WhiteLevel per sample and LinearizationTable.
fn levels(tiff: &Tiff<'_>, ifd: &Ifd, plane: &RawPlane, area: Area) -> Result<Levels, DecodeError> {
    let spp = plane.samples_per_pixel;
    let repeat = match ifd.get(dng_tag::BLACK_LEVEL_REPEAT_DIM) {
        Some(entry) => tiff.unsigned(entry)?,
        None => vec![1, 1],
    };
    let [rows @ 1..=MAX_BLACK_REPEAT, columns @ 1..=MAX_BLACK_REPEAT] = repeat[..] else {
        return Err(DecodeError::corrupt("DNG BlackLevelRepeatDim"));
    };
    let (rows, columns) = (rows as usize, columns as usize);
    let black = numbers(tiff, ifd, dng_tag::BLACK_LEVEL)?.unwrap_or_else(|| vec![0.0]);
    let black = match black.len() {
        1 => vec![black[0]; rows * columns * spp],
        n if n == rows * columns * spp => black,
        _ => return Err(DecodeError::corrupt("DNG BlackLevel count")),
    };
    let deltas = |tag, length: usize| -> Result<Vec<f32>, DecodeError> {
        match numbers(tiff, ifd, tag)? {
            None => Ok(Vec::new()),
            Some(values) if values.len() == length => Ok(to_f32(values)),
            Some(_) => Err(DecodeError::corrupt("DNG BlackLevelDelta count")),
        }
    };
    let default_white = f64::from((1_u32 << plane.bits_per_sample) - 1);
    let white = numbers(tiff, ifd, dng_tag::WHITE_LEVEL)?.unwrap_or_else(|| vec![default_white]);
    let white = match white.len() {
        1 => vec![white[0]; spp],
        n if n == spp => white,
        _ => return Err(DecodeError::corrupt("DNG WhiteLevel count")),
    };
    let linearization = match ifd.get(dng_tag::LINEARIZATION_TABLE) {
        Some(entry) if entry.count > MAX_LINEARIZATION_ENTRIES || entry.count == 0 => {
            return Err(DecodeError::corrupt("DNG LinearizationTable size"));
        }
        Some(entry) => Some(tiff.unsigned(entry)?.iter().map(|&v| v.min(65_535) as u16).collect()),
        None => None,
    };
    Ok(Levels {
        // Samples of a pixel sit side by side, so the block is columns x spp wide.
        black: BlackLevel { width: columns * spp, height: rows, values: to_f32(black) },
        black_origin: (area.left * spp, area.top),
        black_delta_columns: deltas(dng_tag::BLACK_LEVEL_DELTA_H, area.width)?,
        black_delta_rows: deltas(dng_tag::BLACK_LEVEL_DELTA_V, area.height)?,
        white: to_f32(white),
        linearization,
    })
}

fn to_f32(values: Vec<f64>) -> Vec<f32> {
    values.into_iter().map(|v| v as f32).collect()
}

fn cfa_pattern(tiff: &Tiff<'_>, ifd: &Ifd) -> Result<CfaPattern, DecodeError> {
    if tiff.get_u32(ifd, dng_tag::CFA_LAYOUT)?.unwrap_or(RECTANGULAR_LAYOUT) != RECTANGULAR_LAYOUT {
        return Err(DecodeError::unsupported("staggered DNG CFA layouts"));
    }
    let dimensions = match ifd.get(dng_tag::CFA_REPEAT_PATTERN_DIM) {
        Some(entry) => tiff.unsigned(entry)?,
        None => vec![2, 2],
    };
    let [rows, columns] = dimensions[..] else {
        return Err(DecodeError::corrupt("DNG CFARepeatPatternDim"));
    };
    let pattern = ifd
        .get(dng_tag::CFA_PATTERN)
        .ok_or_else(|| DecodeError::corrupt("DNG without CFAPattern"))?;
    let planes = match ifd.get(dng_tag::CFA_PLANE_COLOR) {
        Some(entry) => tiff.unsigned(entry)?,
        None => DEFAULT_PLANE_COLORS.map(u32::from).to_vec(),
    };
    if planes.len() != 3 {
        return Err(DecodeError::unsupported("DNG CFA with other than three colours"));
    }
    let colors = tiff
        .unsigned(pattern)?
        .iter()
        .map(|&plane| match planes.get(plane as usize) {
            Some(&color) if color <= u32::from(BLUE) => Ok(color as u8),
            _ => Err(DecodeError::unsupported("DNG CFA colours other than red, green and blue")),
        })
        .collect::<Result<_, _>>()?;
    CfaPattern::new(columns as usize, rows as usize, colors)
}

fn camera_color(
    tiff: &Tiff<'_>,
    ifd: &Ifd,
    baseline_exposure: f64,
) -> Result<CameraColor, DecodeError> {
    let matrix = |tag| -> Result<Option<Matrix>, DecodeError> {
        match numbers(tiff, ifd, tag)?.as_deref() {
            None => Ok(None),
            Some(&[a, b, c, d, e, f, g, h, i]) => Ok(Some([[a, b, c], [d, e, f], [g, h, i]])),
            Some(_) => {
                Err(DecodeError::unsupported("DNG colour matrices for other than three colours"))
            }
        }
    };
    let calibration = |index: usize| -> Result<Option<Calibration>, DecodeError> {
        let Some(color_matrix) = matrix(dng_tag::COLOR_MATRIX[index])? else {
            return Ok(None);
        };
        let illuminant = tiff.get_u32(ifd, dng_tag::CALIBRATION_ILLUMINANT[index])?;
        Ok(Some(Calibration {
            temperature: illuminant
                .and_then(mirante_raw::color::illuminant_temperature)
                .unwrap_or(f64::NAN),
            color_matrix,
            camera_calibration: matrix(dng_tag::CAMERA_CALIBRATION[index])?.unwrap_or(IDENTITY),
            forward_matrix: matrix(dng_tag::FORWARD_MATRIX[index])?,
        }))
    };
    let first =
        calibration(0)?.ok_or_else(|| DecodeError::corrupt("colour DNG without ColorMatrix1"))?;
    // Interpolation needs both temperatures; otherwise the first matrix stands alone.
    let second = calibration(1)?
        .filter(|second| first.temperature.is_finite() && second.temperature.is_finite());
    let three = |tag| -> Result<Option<[f64; 3]>, DecodeError> {
        Ok(numbers(tiff, ifd, tag)?.and_then(|values| values.try_into().ok()))
    };
    let as_shot =
        match (three(dng_tag::AS_SHOT_NEUTRAL)?, numbers(tiff, ifd, dng_tag::AS_SHOT_WHITE_XY)?) {
            (Some(neutral), _) => AsShot::Neutral(neutral),
            (None, Some(xy)) if xy.len() == 2 => AsShot::WhiteXy((xy[0], xy[1])),
            _ => AsShot::Unknown,
        };
    Ok(CameraColor {
        calibration: first,
        second_calibration: second,
        analog_balance: three(dng_tag::ANALOG_BALANCE)?.unwrap_or([1.0; 3]),
        as_shot,
        baseline_exposure,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Value, tiff};
    use mirante_core::ColorSpace;

    const BLACK: f64 = 64.0;
    const WHITE: f64 = 4095.0;
    /// D65 white as seen by a camera whose ColorMatrix is the identity (XYZ).
    const NEUTRAL: [f64; 3] = [0.9505, 1.0, 1.089];
    const GREY: f64 = 0.4;

    /// Raw value of a grey patch at GREY of white, for colour plane `color`.
    fn grey_sample(color: usize) -> f64 {
        let largest = NEUTRAL.iter().copied().fold(0.0, f64::max);
        BLACK + (WHITE - BLACK) * GREY * NEUTRAL[color] / largest
    }

    fn base_fields(width: u16, height: u16, extra: Vec<(u16, Value)>) -> Vec<(u16, Value)> {
        let mut fields = vec![
            (dng_tag::DNG_VERSION, Value::Undefined(vec![1, 4, 0, 0])),
            (tag::IMAGE_WIDTH, Value::Short(vec![width])),
            (tag::IMAGE_LENGTH, Value::Short(vec![height])),
            (dng_tag::BLACK_LEVEL, Value::Short(vec![BLACK as u16])),
            (dng_tag::WHITE_LEVEL, Value::Short(vec![WHITE as u16])),
            (dng_tag::COLOR_MATRIX[0], Value::SRational(IDENTITY.concat())),
            (dng_tag::CALIBRATION_ILLUMINANT[0], Value::Short(vec![21])),
            (dng_tag::AS_SHOT_NEUTRAL, Value::SRational(NEUTRAL.to_vec())),
        ];
        fields.extend(extra);
        fields
    }

    fn cfa_fields(bits: u16, data: Vec<u8>) -> Vec<(u16, Value)> {
        let count = data.len() as u32;
        vec![
            (tag::BITS_PER_SAMPLE, Value::Short(vec![bits])),
            (tag::PHOTOMETRIC, Value::Short(vec![32803])),
            (tag::STRIP_OFFSETS, Value::DataOffset(data)),
            (tag::STRIP_BYTE_COUNTS, Value::Long(vec![count])),
            (dng_tag::CFA_REPEAT_PATTERN_DIM, Value::Short(vec![2, 2])),
            (dng_tag::CFA_PATTERN, Value::Undefined(vec![0, 1, 1, 2])),
        ]
    }

    /// RGGB mosaic samples of a flat grey patch.
    fn grey_mosaic(width: usize, height: usize) -> Vec<u16> {
        let color = |x: usize, y: usize| [[0, 1], [1, 2]][y % 2][x % 2];
        (0..width * height)
            .map(|i| grey_sample(color(i % width, i / width)).round() as u16)
            .collect()
    }

    fn decode_file(file: &[u8]) -> Image {
        decode(&Tiff::parse(file).unwrap()).unwrap()
    }

    fn assert_grey(image: &Image) {
        for pixel in image.pixels() {
            assert!(pixel[..3].iter().all(|v| (f64::from(*v) - GREY).abs() < 0.01), "{pixel:?}");
        }
    }

    #[test]
    fn develops_uncompressed_16_bit_cfa_to_neutral_grey() {
        let data = grey_mosaic(6, 4).iter().flat_map(|v| v.to_le_bytes()).collect();
        let image = decode_file(&tiff(base_fields(6, 4, cfa_fields(16, data))));
        assert_eq!((image.width(), image.height()), (6, 4));
        assert_eq!(*image.color_space(), ColorSpace::LinearRec2020);
        assert_grey(&image);
    }

    #[test]
    fn applies_packed_bits_active_area_crop_and_orientation() {
        // 8x6 sensor, 12-bit packed; the active area skips the first two rows.
        let mut samples = vec![0; 8 * 2];
        samples.extend(grey_mosaic(8, 4));
        let mut packed = Vec::new();
        for pair in samples.chunks(2) {
            let (a, b) = (u32::from(pair[0]), u32::from(pair[1]));
            packed.extend(&((a << 12 | b) << 8).to_be_bytes()[..3]);
        }
        let mut extra = cfa_fields(12, packed);
        extra.extend([
            (dng_tag::ACTIVE_AREA, Value::Short(vec![2, 0, 6, 8])),
            (dng_tag::DEFAULT_CROP_ORIGIN, Value::Short(vec![1, 1])),
            (dng_tag::DEFAULT_CROP_SIZE, Value::Short(vec![5, 2])),
            (tag::ORIENTATION, Value::Short(vec![6])),
        ]);
        let image = decode_file(&tiff(base_fields(8, 6, extra)));
        assert_eq!((image.width(), image.height()), (2, 5));
        assert_grey(&image);
    }

    #[test]
    fn subtracts_repeating_black_pattern_and_deltas() {
        let blacks = [10.0, 20.0, 30.0, 40.0];
        let mut samples = grey_mosaic(4, 4);
        for (i, sample) in samples.iter_mut().enumerate() {
            let (x, y) = (i % 4, i / 4);
            let shift = blacks[(y % 2) * 2 + x % 2] + x as f64;
            *sample = (f64::from(*sample) - BLACK + shift) as u16;
        }
        let data = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut extra = cfa_fields(16, data);
        extra.extend([
            (dng_tag::BLACK_LEVEL_REPEAT_DIM, Value::Short(vec![2, 2])),
            (dng_tag::BLACK_LEVEL_DELTA_H, Value::SRational(vec![0.0, 1.0, 2.0, 3.0])),
        ]);
        let mut fields = base_fields(4, 4, extra);
        for (tag, value) in &mut fields {
            if *tag == dng_tag::BLACK_LEVEL {
                *value = Value::Short(blacks.map(|b| b as u16).to_vec());
            }
        }
        let image = decode_file(&tiff(fields));
        // Lower blacks widen each site's range slightly, hence the looser bound.
        for pixel in image.pixels() {
            assert!(pixel[..3].iter().all(|v| (f64::from(*v) - GREY).abs() < 0.03), "{pixel:?}");
        }
    }

    #[test]
    fn develops_linear_raw() {
        let pixel: Vec<u16> = (0..3).map(|c| grey_sample(c).round() as u16).collect();
        let data: Vec<u8> = pixel.repeat(4).iter().flat_map(|v| v.to_le_bytes()).collect();
        let count = data.len() as u32;
        let extra = vec![
            (tag::BITS_PER_SAMPLE, Value::Short(vec![16, 16, 16])),
            (tag::SAMPLES_PER_PIXEL, Value::Short(vec![3])),
            (tag::PHOTOMETRIC, Value::Short(vec![34892])),
            (tag::STRIP_OFFSETS, Value::DataOffset(data)),
            (tag::STRIP_BYTE_COUNTS, Value::Long(vec![count])),
        ];
        assert_grey(&decode_file(&tiff(base_fields(2, 2, extra))));
    }

    /// A lossless JPEG of 2x2 samples in two interleaved components, all equal to
    /// the predictor's starting value, so every difference is Huffman code '0'.
    fn flat_lossless_jpeg() -> Vec<u8> {
        let mut jpeg = vec![0xFF, 0xD8];
        jpeg.extend([0xFF, 0xC3, 0, 14, 12, 0, 2, 0, 2, 2, 1, 0x11, 0, 2, 0x11, 0]);
        jpeg.extend([0xFF, 0xC4, 0, 20, 0x00, 1]);
        jpeg.extend([0; 15]);
        jpeg.push(0);
        jpeg.extend([0xFF, 0xDA, 0, 10, 2, 1, 0x00, 2, 0x00, 1, 0, 0]);
        jpeg.extend([0x00, 0xFF, 0xD9]);
        jpeg
    }

    #[test]
    fn reads_lossless_jpeg_tiles_with_interleaved_columns() {
        let jpeg = flat_lossless_jpeg();
        let length = jpeg.len() as u32;
        let extra = vec![
            (tag::BITS_PER_SAMPLE, Value::Short(vec![12])),
            (tag::COMPRESSION, Value::Short(vec![7])),
            (tag::PHOTOMETRIC, Value::Short(vec![34892])),
            (tag::TILE_WIDTH, Value::Short(vec![4])),
            (tag::TILE_LENGTH, Value::Short(vec![2])),
            (tag::TILE_OFFSETS, Value::DataOffset(jpeg)),
            (tag::TILE_BYTE_COUNTS, Value::Long(vec![length])),
        ];
        let mut fields = base_fields(3, 2, extra);
        fields.retain(|(t, _)| *t != dng_tag::BLACK_LEVEL);
        let file = tiff(fields);
        let tiff = Tiff::parse(&file).unwrap();
        let plane = unpack::read(&tiff, &tiff.ifds[0]).unwrap();
        assert_eq!(plane.samples, [2048; 6]);
        let image = decode(&tiff).unwrap();
        assert_eq!((image.width(), image.height()), (3, 2));
        let value = image.pixels()[0][0];
        assert!(image.pixels().iter().all(|p| p[..3].iter().all(|v| (v - value).abs() < 1e-6)));
    }

    #[test]
    fn colour_dng_without_matrix_is_corrupt() {
        let data = grey_mosaic(2, 2).iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut fields = base_fields(2, 2, cfa_fields(16, data));
        fields.retain(|(t, _)| *t != dng_tag::COLOR_MATRIX[0]);
        let file = tiff(fields);
        assert!(matches!(decode(&Tiff::parse(&file).unwrap()), Err(DecodeError::Corrupt(_))));
    }
}
