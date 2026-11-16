//! Module: mirante_raw_canon::sraw
//! Purpose: Decodes Canon sRAW/mRAW (subsampled YCbCr lossless JPEG) into
//! camera RGB, three samples per pixel.
//! Created: 2026-11-16
//! Architecture: Ported from dcraw's canon_sraw_load_raw by Dave Coffin:
//! slices are reassembled, chroma is interpolated, and YCbCr becomes RGB scaled
//! by the ColorData sRAW multipliers. The result has no black offset and the
//! camera's 14-bit scale (white 0x3FFF); it matches LibRaw's output exactly.

use crate::ljpeg::LosslessJpeg;
use mirante_core::DecodeError;

const CHROMA_OFFSET: i32 = 16384;
/// Model ids whose sRAW uses the older, hue-corrected conversion.
const HUE_MODELS: [u32; 5] = [0x8000_0218, 0x8000_0250, 0x8000_0261, 0x8000_0281, 0x8000_0287];
const FIRST_MODERN_MODEL: u32 = 0x8000_0281;
const FIRMWARE_MODEL: u32 = 0x8000_0218;
const FIRMWARE_WITH_NEW_HUE: u32 = 1_000_006;
/// Models before this one store luma with a 512 offset.
const FIRST_ZERO_BLACK_MODEL: u32 = 0x8000_0218;
const OLD_LUMA_OFFSET: i32 = 512;

pub struct Camera<'a> {
    pub model_id: u32,
    pub firmware: Option<&'a str>,
    /// R, G, B multipliers in 1/1024 units.
    pub multipliers: [u16; 3],
}

/// Pixels as interleaved RGB.
pub struct Rgb {
    pub width: usize,
    pub height: usize,
    pub samples: Vec<u16>,
}

/// `slices` is the CR2 slice tag: count, width and last width (in JPEG samples).
pub fn decode(
    jpeg: &LosslessJpeg,
    slices: Option<[usize; 3]>,
    camera: &Camera<'_>,
) -> Result<Rgb, DecodeError> {
    let clrs = jpeg.samples_per_mcu;
    if !matches!(jpeg.sraw, 1 | 3) || clrs != jpeg.sraw + 3 {
        return Err(DecodeError::unsupported("Canon sRAW with unusual chroma subsampling"));
    }
    let (width, height) = (jpeg.width, jpeg.height);
    if height > width {
        return Err(DecodeError::unsupported("Canon sRAW stored in portrait"));
    }
    DecodeError::check_size(width as u32, height as u32)?;
    // Y, Cb, Cr per pixel; chroma only at even columns until interpolated.
    let mut ycc = vec![[0_i32; 3]; width * height];
    let mcus = width / 2;
    let row_samples = mcus * clrs;
    if row_samples == 0 {
        return Err(DecodeError::corrupt("Canon sRAW is too narrow"));
    }
    let mut rows = jpeg.rows(mcus);
    let mut samples: Vec<u16> = Vec::new();
    let mut at = 0;
    let (count, slice_width) = slices.map_or((0, 0), |s| (s[0], s[1]));
    let mut end = 0;
    for _ in 0..=count {
        let start = end;
        end += slice_width * 2 / clrs;
        if count == 0 || end + 1 > width {
            end = width & !1;
        }
        for row in (0..height).step_by(clrs / 2 - 1) {
            for column in (start..end).step_by(2) {
                at %= row_samples;
                if at == 0 {
                    samples.clear();
                    samples.extend_from_slice(rows.next_row()?);
                }
                if column < width {
                    let mcu = &samples[at..at + clrs];
                    for (c, &luma) in mcu[..clrs - 2].iter().enumerate() {
                        let (y, x) = (row + (c >> 1), column + (c & 1));
                        if y < height && x < width {
                            ycc[y * width + x][0] = i32::from(luma as i16);
                        }
                    }
                    let pixel = &mut ycc[row * width + column];
                    pixel[1] = chroma(mcu[clrs - 2]);
                    pixel[2] = chroma(mcu[clrs - 1]);
                }
                at += clrs;
            }
        }
    }
    interpolate_chroma(&mut ycc, width, height, jpeg.sraw);
    let samples = ycc.iter().flat_map(|&pixel| to_rgb(pixel, jpeg.sraw, camera)).collect();
    Ok(Rgb { width, height, samples })
}

/// Chroma is stored offset by 16384 and kept as a C short, as dcraw does.
fn chroma(value: u16) -> i32 {
    i32::from((i32::from(value) - CHROMA_OFFSET) as i16)
}

fn interpolate_chroma(ycc: &mut [[i32; 3]], width: usize, height: usize, sraw: usize) {
    // Chroma of a pixel: copied from one neighbour, or the rounded mean of two.
    let fill = |ycc: &mut [[i32; 3]], at: usize, first: usize, second: Option<usize>| {
        let (a, b) = (ycc[first], ycc[second.unwrap_or(first)]);
        for c in 1..3 {
            ycc[at][c] = if second.is_some() { (a[c] + b[c] + 1) >> 1 } else { a[c] };
        }
    };
    for row in 0..height {
        let line = row * width;
        if row & (sraw >> 1) != 0 {
            for at in (line..line + width).step_by(2) {
                fill(ycc, at, at - width, (row + 1 < height).then_some(at + width));
            }
        }
        for at in (line + 1..line + width).step_by(2) {
            fill(ycc, at, at - 1, (at + 1 < line + width).then_some(at + 1));
        }
    }
}

fn to_rgb([mut y, mut cb, mut cr]: [i32; 3], sraw: usize, camera: &Camera<'_>) -> [u16; 3] {
    let pixel = if HUE_MODELS.contains(&camera.model_id) {
        let new_hue = camera.model_id >= FIRST_MODERN_MODEL
            || (camera.model_id == FIRMWARE_MODEL
                && camera
                    .firmware
                    .and_then(firmware_version)
                    .is_some_and(|v| v > FIRMWARE_WITH_NEW_HUE));
        let hue = if new_hue { (sraw as i32) << 1 } else { (sraw as i32 + 1) << 2 };
        cb = i32::from(((cb << 2) + hue) as i16);
        cr = i32::from(((cr << 2) + hue) as i16);
        [
            y + ((50 * cb + 22929 * cr) >> 14),
            y + ((-5640 * cb - 11751 * cr) >> 14),
            y + ((29040 * cb - 101 * cr) >> 14),
        ]
    } else {
        if camera.model_id < FIRST_ZERO_BLACK_MODEL {
            y = i32::from((y - OLD_LUMA_OFFSET) as i16);
        }
        [y + cr, y + ((-778 * cb - (cr << 11)) >> 12), y + cb]
    };
    std::array::from_fn(|c| {
        let scaled = (i64::from(pixel[c]) * i64::from(camera.multipliers[c])) >> 10;
        scaled.clamp(0, i64::from(u16::MAX)) as u16
    })
}

/// "Firmware Version 1.0.6" as 1_000_006, like dcraw's comparison.
fn firmware_version(text: &str) -> Option<u32> {
    let digits = &text[text.find(|c: char| c.is_ascii_digit())?..];
    let mut parts = digits.split('.').map(|part| {
        let end = part.find(|c: char| !c.is_ascii_digit()).unwrap_or(part.len());
        part[..end].parse::<u32>().unwrap_or(0)
    });
    let mut next = || parts.next().unwrap_or(0);
    Some((next() * 1000 + next()) * 1000 + next())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_odd_columns_and_converts() {
        let mut ycc = vec![[100, 10, 20], [100, 0, 0], [100, 30, 40], [100, 0, 0]];
        interpolate_chroma(&mut ycc, 4, 1, 1);
        assert_eq!(ycc[1], [100, 20, 30]);
        assert_eq!(ycc[3], [100, 30, 40]);
        let camera =
            Camera { model_id: 0x8000_0285, firmware: None, multipliers: [1024, 2048, 512] };
        let rgb = to_rgb([1000, 100, 200], 1, &camera);
        let green = 2 * (1000 + ((-77_800_i32 - 409_600) >> 12));
        assert_eq!(rgb, [1200, green as u16, 550]);
    }

    #[test]
    fn reads_firmware_versions() {
        assert_eq!(firmware_version("Firmware Version 1.0.7"), Some(1_000_007));
        assert_eq!(firmware_version("Firmware 2.1"), Some(2_001_000));
        assert_eq!(firmware_version("none"), None);
    }
}
