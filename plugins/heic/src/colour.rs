//! Module: mirante_format_heic::colour
//! Purpose: Tags decoded RGB with the item's colour space (ICC or nclx).
//! Created: 2026-11-16
//! Architecture: ColorSpace has no tag for P3, BT.2020, PQ or HLG, so such nclx
//! pictures are linearised and converted to LinearRec2020 here; sRGB-like ones are
//! only tagged. An ICC profile always wins over nclx, as in libheif.

use crate::properties::Nclx;
use mirante_core::{ColorSpace, DecodeError, Image};
use std::sync::Arc;

/// H.273 ColourPrimaries.
mod primaries {
    pub const BT709: u16 = 1;
    pub const UNSPECIFIED: u16 = 2;
    pub const BT470BG: u16 = 5;
    pub const BT601: u16 = 6;
    pub const SMPTE240: u16 = 7;
    pub const BT2020: u16 = 9;
    pub const P3_D65: u16 = 12;
}

/// H.273 TransferCharacteristics.
mod transfer {
    pub const BT709: u16 = 1;
    pub const UNSPECIFIED: u16 = 2;
    pub const GAMMA22: u16 = 4;
    pub const GAMMA28: u16 = 5;
    pub const BT601: u16 = 6;
    pub const LINEAR: u16 = 8;
    pub const SRGB: u16 = 13;
    pub const BT2020_10BIT: u16 = 14;
    pub const BT2020_12BIT: u16 = 15;
    pub const PQ: u16 = 16;
    pub const HLG: u16 = 18;
}

/// Linear BT.709 and Display P3 to linear BT.2020, both D65 (BT.2087 style).
const BT709_TO_BT2020: [[f32; 3]; 3] = [
    [0.627_404, 0.329_283, 0.043_313],
    [0.069_097, 0.919_540, 0.011_362],
    [0.016_391, 0.088_013, 0.895_595],
];
const P3_TO_BT2020: [[f32; 3]; 3] = [
    [0.753_833, 0.198_597, 0.047_570],
    [0.045_744, 0.941_777, 0.012_479],
    [-0.001_210, 0.017_602, 0.983_609],
];
/// PQ is absolute; SDR reference white (BT.2408) becomes 1.0.
const PQ_PEAK_NITS: f32 = 10_000.0;
const REFERENCE_WHITE_NITS: f32 = 203.0;
/// HLG reference white is a 75% signal; scene light at that level becomes 1.0.
const HLG_REFERENCE_SIGNAL: f32 = 0.75;

/// libheif's assumption for items without a `colr` box.
pub const DEFAULT_NCLX: Nclx =
    Nclx { primaries: primaries::BT709, transfer: transfer::SRGB, matrix: 6, full_range: true };

/// The colour description of one item, from its `colr` properties.
#[derive(Clone, Debug, Default)]
pub struct ColourInfo {
    pub nclx: Option<Nclx>,
    pub icc: Option<Arc<[u8]>>,
}

impl ColourInfo {
    /// Fills whatever this item does not declare from another (a grid's) description.
    pub fn or(&self, fallback: &Self) -> Self {
        Self {
            nclx: self.nclx.or(fallback.nclx),
            icc: self.icc.clone().or_else(|| fallback.icc.clone()),
        }
    }

    pub fn matrix_nclx(&self) -> Nclx {
        self.nclx.unwrap_or(DEFAULT_NCLX)
    }

    /// Tags (or converts) RGB that is still in the item's coded encoding.
    pub fn finish(&self, image: Image) -> Result<Image, DecodeError> {
        if let Some(icc) = &self.icc {
            return Ok(image.with_color_space(ColorSpace::Icc(icc.clone())));
        }
        let Some(nclx) = self.nclx else {
            return Ok(image);
        };
        // A JPEG item may carry its own embedded profile; trust it over nclx.
        if *image.color_space() != ColorSpace::Srgb {
            return Ok(image);
        }
        let curve = Curve::of(nclx.transfer)?;
        let gamut = Gamut::of(nclx.primaries)?;
        match (gamut, curve) {
            (Gamut::Bt709, Curve::Srgb) => Ok(image),
            (Gamut::Bt709, Curve::Linear) => Ok(image.with_color_space(ColorSpace::LinearSrgb)),
            _ => Ok(convert(image, curve, gamut)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Curve {
    /// sRGB and the BT.709/BT.601 camera curves, which viewers treat alike.
    Srgb,
    Linear,
    Gamma(f32),
    Pq,
    Hlg,
}

impl Curve {
    fn of(code: u16) -> Result<Self, DecodeError> {
        Ok(match code {
            transfer::BT709
            | transfer::UNSPECIFIED
            | transfer::BT601
            | transfer::SRGB
            | transfer::BT2020_10BIT
            | transfer::BT2020_12BIT => Self::Srgb,
            transfer::LINEAR => Self::Linear,
            transfer::GAMMA22 => Self::Gamma(2.2),
            transfer::GAMMA28 => Self::Gamma(2.8),
            transfer::PQ => Self::Pq,
            transfer::HLG => Self::Hlg,
            other => {
                return Err(DecodeError::unsupported(format!("transfer characteristics {other}")));
            }
        })
    }

    fn to_linear(self, value: f32) -> f32 {
        match self {
            Self::Srgb if value <= 0.040_45 => value / 12.92,
            Self::Srgb => ((value + 0.055) / 1.055).powf(2.4),
            Self::Linear => value,
            Self::Gamma(gamma) => value.max(0.0).powf(gamma),
            Self::Pq => pq_to_linear(value) * PQ_PEAK_NITS / REFERENCE_WHITE_NITS,
            Self::Hlg => hlg_to_linear(value) / hlg_to_linear(HLG_REFERENCE_SIGNAL),
        }
    }
}

fn pq_to_linear(value: f32) -> f32 {
    const M1: f32 = 2610.0 / 16384.0;
    const M2: f32 = 2523.0 / 4096.0 * 128.0;
    const C1: f32 = 3424.0 / 4096.0;
    const C2: f32 = 2413.0 / 4096.0 * 32.0;
    const C3: f32 = 2392.0 / 4096.0 * 32.0;
    let power = value.max(0.0).powf(1.0 / M2);
    ((power - C1).max(0.0) / (C2 - C3 * power)).powf(1.0 / M1)
}

fn hlg_to_linear(value: f32) -> f32 {
    const A: f32 = 0.178_832_77;
    const B: f32 = 0.284_668_92;
    const C: f32 = 0.559_910_7;
    if value <= 0.5 { value * value / 3.0 } else { (((value - C) / A).exp() + B) / 12.0 }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Gamut {
    Bt709,
    Bt2020,
    P3,
}

impl Gamut {
    fn of(code: u16) -> Result<Self, DecodeError> {
        Ok(match code {
            // SD primaries differ from BT.709 by less than viewers distinguish.
            primaries::BT709
            | primaries::UNSPECIFIED
            | primaries::BT470BG
            | primaries::BT601
            | primaries::SMPTE240 => Self::Bt709,
            primaries::BT2020 => Self::Bt2020,
            primaries::P3_D65 => Self::P3,
            other => return Err(DecodeError::unsupported(format!("colour primaries {other}"))),
        })
    }
}

fn convert(mut image: Image, curve: Curve, gamut: Gamut) -> Image {
    let matrix = match gamut {
        Gamut::Bt709 => Some(BT709_TO_BT2020),
        Gamut::P3 => Some(P3_TO_BT2020),
        Gamut::Bt2020 => None,
    };
    for pixel in image.pixels_mut() {
        let rgb = [pixel[0], pixel[1], pixel[2]].map(|v| curve.to_linear(v));
        let rgb = match matrix {
            Some(m) => m.map(|row| row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2]),
            None => rgb,
        };
        pixel[..3].copy_from_slice(&rgb);
    }
    image.with_color_space(ColorSpace::LinearRec2020)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(value: [f32; 3]) -> Image {
        Image::new(1, 1, vec![[value[0], value[1], value[2], 1.0]], ColorSpace::Srgb).unwrap()
    }

    fn info(primaries: u16, transfer: u16) -> ColourInfo {
        ColourInfo { nclx: Some(Nclx { primaries, transfer, ..DEFAULT_NCLX }), icc: None }
    }

    #[test]
    fn srgb_nclx_only_tags() {
        let image = info(1, 13).finish(pixel([0.5; 3])).unwrap();
        assert_eq!(image.pixels()[0][0], 0.5);
        assert_eq!(*image.color_space(), ColorSpace::Srgb);
    }

    #[test]
    fn icc_wins_over_nclx() {
        let mut colour = info(12, 16);
        colour.icc = Some(Arc::from(&b"icc"[..]));
        let image = colour.finish(pixel([0.5; 3])).unwrap();
        assert_eq!(*image.color_space(), ColorSpace::Icc(Arc::from(&b"icc"[..])));
    }

    #[test]
    fn p3_white_stays_white_in_rec2020() {
        let image = info(12, 13).finish(pixel([1.0; 3])).unwrap();
        assert_eq!(*image.color_space(), ColorSpace::LinearRec2020);
        assert!(image.pixels()[0][..3].iter().all(|v| (v - 1.0).abs() < 1e-3));
    }

    #[test]
    fn pq_reference_white_is_one() {
        // 203 cd/m2 encodes to a PQ signal of about 0.5806.
        let image = info(9, 16).finish(pixel([0.580_7; 3])).unwrap();
        assert!((image.pixels()[0][1] - 1.0).abs() < 0.01);
    }

    #[test]
    fn unknown_transfer_is_reported() {
        assert!(info(1, 17).finish(pixel([0.5; 3])).is_err());
    }
}
