//! Module: mirante_format_jpeg::output
//! Purpose: Turns decoded component planes into RGBA pixels.
//! Created: 2026-11-16
//! Architecture: Chroma is upsampled bilinearly with centred samples, which for
//! 2:1 ratios equals libjpeg's "fancy" triangle filter; colour models are then
//! converted with mirante-codec's shared formulas.

use mirante_codec::color_models::{cmyk_to_rgb, ycbcr_to_rgb};
use mirante_core::Rgba;

/// Bilinear weights are fixed point with this many fractional bits.
const WEIGHT_BITS: u32 = 8;
const WEIGHT_ONE: u32 = 1 << WEIGHT_BITS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorModel {
    Gray,
    YCbCr,
    Rgb,
    Cmyk,
    Ycck,
}

/// One component's samples; only the top-left `width` x `height` are image data.
pub struct Plane {
    pub samples: Vec<u8>,
    pub stride: usize,
    pub width: usize,
    pub height: usize,
}

impl Plane {
    /// Resamples to `out_width` x `out_height`, given the component's sampling
    /// factors (`h`, `v`) relative to the frame's largest (`h_max`, `v_max`).
    pub fn upsample(
        &self,
        (h, v): (usize, usize),
        (h_max, v_max): (usize, usize),
        (out_width, out_height): (usize, usize),
    ) -> Vec<u8> {
        let mut out = Vec::with_capacity(out_width * out_height);

        if h == h_max && v == v_max {
            for row in self.samples.chunks(self.stride).take(out_height) {
                out.extend_from_slice(&row[..out_width]);
            }
            return out;
        }
        let columns = taps(out_width, self.width, h, h_max);
        let rows = taps(out_height, self.height, v, v_max);

        for &(top, bottom, weight_y) in &rows {
            let top = &self.samples[top * self.stride..];
            let bottom = &self.samples[bottom * self.stride..];
            out.extend(columns.iter().map(|&(left, right, weight_x)| {
                let mix = |line: &[u8]| {
                    u32::from(line[left]) * (WEIGHT_ONE - weight_x)
                        + u32::from(line[right]) * weight_x
                };
                let value = mix(top) * (WEIGHT_ONE - weight_y) + mix(bottom) * weight_y;
                ((value + (1 << (2 * WEIGHT_BITS - 1))) >> (2 * WEIGHT_BITS)) as u8
            }));
        }
        out
    }
}

/// For each output index, the two source samples around it and the weight of the second.
fn taps(out_len: usize, in_len: usize, factor: usize, max: usize) -> Vec<(usize, usize, u32)> {
    let last = in_len.saturating_sub(1) as f32;
    (0..out_len)
        .map(|o| {
            let position = (o as f32 + 0.5) * factor as f32 / max as f32 - 0.5;
            let base = position.floor();
            let weight = ((position - base) * WEIGHT_ONE as f32).round() as u32;
            let first = base.clamp(0.0, last) as usize;
            let second = (base + 1.0).clamp(0.0, last) as usize;
            (first, second, weight)
        })
        .collect()
}

/// Converts full-resolution component samples to RGBA. `inverted` marks Adobe
/// CMYK, which stores 255 for no ink.
pub fn convert(model: ColorModel, planes: &[Vec<u8>], inverted: bool) -> Vec<Rgba> {
    let unit: [f32; 256] = std::array::from_fn(|v| v as f32 / 255.0);
    // JPEG's neutral chroma is the sample value 128.
    let chroma: [f32; 256] = std::array::from_fn(|v| (v as f32 - 128.0) / 255.0);
    let ink = |v: u8| if inverted { 1.0 - unit[usize::from(v)] } else { unit[usize::from(v)] };
    let ycc = |i: usize| {
        let [y, cb, cr] = [0, 1, 2].map(|c| usize::from(planes[c][i]));
        ycbcr_to_rgb(unit[y], chroma[cb], chroma[cr])
    };
    let count = planes[0].len();

    (0..count)
        .map(|i| {
            let [r, g, b] = match model {
                ColorModel::Gray => [unit[usize::from(planes[0][i])]; 3],
                ColorModel::YCbCr => ycc(i),
                ColorModel::Rgb => [0, 1, 2].map(|c| unit[usize::from(planes[c][i])]),
                ColorModel::Cmyk => cmyk_to_rgb([0, 1, 2, 3].map(|c| ink(planes[c][i]))),
                ColorModel::Ycck => {
                    // YCC encodes 1 - stored CMY, so the ink is the RGB value when inverted.
                    let [r, g, b] = ycc(i);
                    let cmy = if inverted { [r, g, b] } else { [1.0 - r, 1.0 - g, 1.0 - b] };
                    cmyk_to_rgb([cmy[0], cmy[1], cmy[2], ink(planes[3][i])])
                }
            };
            [r, g, b, 1.0]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubling_uses_the_triangle_filter() {
        let plane = Plane { samples: vec![0, 100, 0, 0], stride: 4, width: 2, height: 1 };
        let out = plane.upsample((1, 1), (2, 1), (4, 1));
        assert_eq!(out, vec![0, 25, 75, 100]);
    }
}
