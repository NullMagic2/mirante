//! Module: mirante_format_heic::ycbcr
//! Purpose: Converts decoded YCbCr planes to RGB with the nclx matrix and range.
//! Created: 2026-11-16
//! Architecture: Chroma is upsampled bilinearly at the HEVC default sample position
//! (co-sited horizontally, centred vertically). Output keeps the coded transfer
//! curve; colour.rs decides what colour space that is.

use crate::planes::{Plane, Planes};
use crate::properties::Nclx;
use mirante_core::{ColorSpace, DecodeError, Image, Rgba};

/// H.273 MatrixCoefficients code points this converter understands.
mod matrix {
    pub const IDENTITY: u16 = 0;
    pub const BT709: u16 = 1;
    pub const UNSPECIFIED: u16 = 2;
    pub const FCC: u16 = 4;
    pub const BT470BG: u16 = 5;
    pub const BT601: u16 = 6;
    pub const SMPTE240: u16 = 7;
    pub const BT2020_NCL: u16 = 9;
    pub const BT2020_CL: u16 = 10;
    pub const CHROMATICITY_NCL: u16 = 12;
}

/// H.273 ColourPrimaries code points with known luma weights.
const PRIMARIES_BT2020: u16 = 9;
const PRIMARIES_P3_D65: u16 = 12;

/// How luma and chroma combine into RGB.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Coefficients {
    /// Y holds green, Cb blue and Cr red (GBR coding).
    Identity,
    /// Red and blue luma weights; green is the remainder.
    Luma { kr: f32, kb: f32 },
}

fn coefficients(nclx: &Nclx) -> Result<Coefficients, DecodeError> {
    let luma = |kr, kb| Ok(Coefficients::Luma { kr, kb });
    match nclx.matrix {
        matrix::IDENTITY => Ok(Coefficients::Identity),
        matrix::BT709 => luma(0.2126, 0.0722),
        matrix::FCC => luma(0.30, 0.11),
        matrix::UNSPECIFIED | matrix::BT470BG | matrix::BT601 => luma(0.299, 0.114),
        matrix::SMPTE240 => luma(0.212, 0.087),
        // Constant-luminance BT.2020 is rare in stills; the NCL weights are close.
        matrix::BT2020_NCL | matrix::BT2020_CL => luma(0.2627, 0.0593),
        matrix::CHROMATICITY_NCL => match nclx.primaries {
            PRIMARIES_BT2020 => luma(0.2627, 0.0593),
            PRIMARIES_P3_D65 => luma(0.2290, 0.0793),
            _ => luma(0.2126, 0.0722),
        },
        other => Err(DecodeError::unsupported(format!("YCbCr matrix coefficients {other}"))),
    }
}

/// Maps samples to 0..1 (luma) or -0.5..0.5 (chroma) for the signalled range.
#[derive(Clone, Copy)]
struct Range {
    offset: f32,
    scale: f32,
}

impl Range {
    fn new(plane: &Plane, full_range: bool, chroma: bool) -> Self {
        let step = (1_u32 << plane.bit_depth.saturating_sub(8)) as f32;
        let half = (1_u32 << (plane.bit_depth - 1)) as f32;
        match (full_range, chroma) {
            (true, false) => Self { offset: 0.0, scale: plane.max_value() },
            (true, true) => Self { offset: half, scale: plane.max_value() },
            (false, false) => Self { offset: 16.0 * step, scale: 219.0 * step },
            (false, true) => Self { offset: half, scale: 224.0 * step },
        }
    }

    fn apply(self, value: f32) -> f32 {
        (value - self.offset) / self.scale
    }
}

/// Precomputed bilinear taps along one axis: two source indices and a weight.
fn taps(length: u32, plane_length: u32, shift: u32, centred: bool) -> Vec<(u32, u32, f32)> {
    let last = plane_length.saturating_sub(1) as f32;
    (0..length)
        .map(|i| {
            let mut position = i as f32 / (1 << shift) as f32;
            if shift > 0 && centred {
                position -= 0.25;
            }
            let position = position.clamp(0.0, last);
            let low = position.floor();
            (low as u32, (low as u32 + 1).min(last as u32), position - low)
        })
        .collect()
}

/// Converts the top-left `width` x `height` of the planes to RGB, alpha 1.
pub fn to_image(
    planes: &Planes,
    width: u32,
    height: u32,
    nclx: &Nclx,
) -> Result<Image, DecodeError> {
    let count = DecodeError::check_size(width, height)?;
    if width > planes.width() || height > planes.height() {
        return Err(DecodeError::corrupt("decoded picture is smaller than its item"));
    }
    let luma_range = Range::new(&planes.luma, nclx.full_range, false);
    let mut pixels: Vec<Rgba> = Vec::with_capacity(count);

    let Some([cb_plane, cr_plane]) = &planes.chroma else {
        for y in 0..height {
            for x in 0..width {
                let grey = luma_range.apply(f32::from(planes.luma.get(x, y))).clamp(0.0, 1.0);
                pixels.push([grey, grey, grey, 1.0]);
            }
        }
        return Ok(Image::new(width, height, pixels, ColorSpace::Srgb)?);
    };
    let coefficients = coefficients(nclx)?;
    let identity = coefficients == Coefficients::Identity;
    let chroma_range = Range::new(cb_plane, nclx.full_range, !identity);
    let (sx, sy) = planes.format.shifts();
    let columns = taps(width, cb_plane.width, sx, false);
    let rows = taps(height, cb_plane.height, sy, true);
    let sample = |plane: &Plane, (y0, y1, fy): (u32, u32, f32), (x0, x1, fx): (u32, u32, f32)| {
        let at = |x, y| f32::from(plane.get(x, y));
        let top = at(x0, y0) + (at(x1, y0) - at(x0, y0)) * fx;
        let bottom = at(x0, y1) + (at(x1, y1) - at(x0, y1)) * fx;
        chroma_range.apply(top + (bottom - top) * fy)
    };

    for (y, &row) in rows.iter().enumerate() {
        for (x, &column) in columns.iter().enumerate() {
            let luma = luma_range.apply(f32::from(planes.luma.get(x as u32, y as u32)));
            let (cb, cr) = (sample(cb_plane, row, column), sample(cr_plane, row, column));
            let rgb = match coefficients {
                Coefficients::Identity => [cr, luma, cb],
                Coefficients::Luma { kr, kb } => {
                    let red = luma + (2.0 - 2.0 * kr) * cr;
                    let blue = luma + (2.0 - 2.0 * kb) * cb;
                    [red, (luma - kr * red - kb * blue) / (1.0 - kr - kb), blue]
                }
            };
            let [r, g, b] = rgb.map(|v| v.clamp(0.0, 1.0));
            pixels.push([r, g, b, 1.0]);
        }
    }
    Ok(Image::new(width, height, pixels, ColorSpace::Srgb)?)
}

/// The luma plane alone, normalised to 0..1, as used for alpha planes.
pub fn luma_values(
    planes: &Planes,
    width: u32,
    height: u32,
    full_range: bool,
) -> Result<Vec<f32>, DecodeError> {
    DecodeError::check_size(width, height)?;
    if width > planes.width() || height > planes.height() {
        return Err(DecodeError::corrupt("decoded picture is smaller than its item"));
    }
    let range = Range::new(&planes.luma, full_range, false);
    let values = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| range.apply(f32::from(planes.luma.get(x, y))).clamp(0.0, 1.0))
        .collect();
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planes::ChromaFormat;

    const FULL_601: Nclx = Nclx { primaries: 1, transfer: 13, matrix: 6, full_range: true };

    fn planes_420(luma: u16, cb: u16, cr: u16, size: u32) -> Planes {
        let count = (size * size) as usize;
        let half = size.div_ceil(2);
        let chroma =
            |value| Plane::new(half, half, 8, vec![value; (half * half) as usize]).unwrap();
        let luma = Plane::new(size, size, 8, vec![luma; count]).unwrap();
        Planes::new(ChromaFormat::Yuv420, luma, Some([chroma(cb), chroma(cr)])).unwrap()
    }

    fn close(a: [f32; 4], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(a, b)| (a - b).abs() < 0.01)
    }

    #[test]
    fn neutral_chroma_gives_grey() {
        let image = to_image(&planes_420(128, 128, 128, 5), 5, 5, &FULL_601).unwrap();
        assert!(image.pixels().iter().all(|&p| close(p, [128.0 / 255.0; 3])));
    }

    #[test]
    fn converts_bt601_red_full_and_limited_range() {
        // JFIF red: Y 76, Cb 85, Cr 255.
        let image = to_image(&planes_420(76, 85, 255, 4), 3, 3, &FULL_601).unwrap();
        assert!(close(image.pixels()[4], [1.0, 0.0, 0.0]));
        let limited = Nclx { full_range: false, matrix: 1, ..FULL_601 };
        let image = to_image(&planes_420(63, 102, 240, 4), 4, 4, &limited).unwrap();
        assert!(close(image.pixels()[5], [1.0, 0.0, 0.0]));
    }

    #[test]
    fn upsamples_chroma_between_samples() {
        let luma = Plane::new(4, 2, 8, vec![128; 8]).unwrap();
        let cb = Plane::new(2, 1, 8, vec![128, 128]).unwrap();
        let cr = Plane::new(2, 1, 8, vec![128, 178]).unwrap();
        let planes = Planes::new(ChromaFormat::Yuv420, luma, Some([cb, cr])).unwrap();
        let image = to_image(&planes, 4, 2, &FULL_601).unwrap();
        let reds: Vec<f32> = image.row(0).iter().map(|p| p[0]).collect();
        assert!(reds[0] < reds[1] && reds[1] < reds[2] && reds[2] <= reds[3]);
        assert!((reds[1] - (reds[0] + reds[2]) / 2.0).abs() < 1e-4);
    }

    #[test]
    fn identity_matrix_reads_gbr() {
        let plane = |v| Plane::new(1, 1, 10, vec![v]).unwrap();
        let planes =
            Planes::new(ChromaFormat::Yuv444, plane(1023), Some([plane(0), plane(511)])).unwrap();
        let gbr = Nclx { matrix: 0, ..FULL_601 };
        let pixel = to_image(&planes, 1, 1, &gbr).unwrap().pixels()[0];
        assert!(close(pixel, [511.0 / 1023.0, 1.0, 0.0]));
    }

    #[test]
    fn crops_to_the_item_size_and_rejects_small_pictures() {
        let planes = planes_420(16, 128, 128, 6);
        assert_eq!(to_image(&planes, 5, 3, &FULL_601).unwrap().width(), 5);
        assert!(to_image(&planes, 7, 3, &FULL_601).is_err());
        let alpha = luma_values(&planes, 2, 2, false).unwrap();
        assert_eq!(alpha, [0.0; 4]);
    }
}
