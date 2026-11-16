//! Module: mirante_edit::adjust
//! Purpose: Exposure, contrast, saturation and white balance corrections.
//! Created: 2026-11-16
//! Architecture: Each slider is -1..1 except exposure (EV). The maths runs in
//! linear Rec. 2020; contrast pivots on 18% grey so midtones stay put.

use mirante_core::{Image, Rgba};

const MID_GREY: f32 = 0.18;
/// Rec. 2020 luminance weights.
const LUMA: [f32; 3] = [0.2627, 0.6780, 0.0593];
/// Full temperature or tint travel, in stops of channel gain.
const WHITE_BALANCE_RANGE: f32 = 0.5;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Adjustments {
    pub exposure: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub temperature: f32,
    pub tint: f32,
}

impl Adjustments {
    pub fn apply(&self, image: &mut Image) {
        if *self == Self::default() {
            return;
        }
        let gains = self.channel_gains();
        let contrast = 1.0 + self.contrast;
        let saturation = 1.0 + self.saturation;

        for pixel in image.pixels_mut() {
            *pixel = adjust_pixel(*pixel, gains, contrast, saturation);
        }
    }

    /// Exposure and white balance combined into one multiplier per channel.
    fn channel_gains(&self) -> [f32; 3] {
        let warm = self.temperature * WHITE_BALANCE_RANGE;
        let magenta = self.tint * WHITE_BALANCE_RANGE;
        [warm, -magenta, -warm].map(|stops| (self.exposure + stops).exp2())
    }
}

fn adjust_pixel([r, g, b, a]: Rgba, gains: [f32; 3], contrast: f32, saturation: f32) -> Rgba {
    let balanced = [r * gains[0], g * gains[1], b * gains[2]];
    let contrasted =
        balanced.map(|v| if v > 0.0 { MID_GREY * (v / MID_GREY).powf(contrast) } else { v });
    let luma: f32 = contrasted.iter().zip(LUMA).map(|(v, w)| v * w).sum();
    let [r, g, b] = contrasted.map(|v| luma + (v - luma) * saturation);
    [r, g, b, a]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_stop_doubles_light() {
        let adjustments = Adjustments { exposure: 1.0, ..Default::default() };
        let out = adjust_pixel([0.1, 0.2, 0.3, 1.0], adjustments.channel_gains(), 1.0, 1.0);
        let expected = [0.2, 0.4, 0.6, 1.0];
        assert!(out.iter().zip(expected).all(|(v, e)| (v - e).abs() < 1e-6));
    }

    #[test]
    fn contrast_keeps_mid_grey() {
        let out = adjust_pixel([MID_GREY; 4], [1.0; 3], 1.8, 1.0);
        assert!((out[0] - MID_GREY).abs() < 1e-6);
    }

    #[test]
    fn zero_saturation_is_grey() {
        let out = adjust_pixel([0.8, 0.2, 0.1, 1.0], [1.0; 3], 1.0, 0.0);
        assert!((out[0] - out[1]).abs() < 1e-6 && (out[1] - out[2]).abs() < 1e-6);
    }
}
