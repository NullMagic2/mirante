//! Module: mirante_raw::frame
//! Purpose: NativeRaw, the contract between maker decoders and development.
//! Created: 2026-11-16
//! Architecture: Maker crates fill it with undeveloped photosites and what the
//! file says about them; they never demosaic or convert colour themselves.

use crate::{
    demosaic::{BLUE, CfaPattern, RED},
    unpack::RawPlane,
};

/// A rectangle of photosites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Area {
    pub left: usize,
    pub top: usize,
    pub width: usize,
    pub height: usize,
}

/// Black level as a repeating block, aligned to the plane's origin (0, 0).
/// A single value is a 1x1 block.
#[derive(Clone, Debug, PartialEq)]
pub struct BlackLevel {
    pub width: usize,
    pub height: usize,
    pub values: Vec<f32>,
}

impl BlackLevel {
    pub fn uniform(value: f32) -> Self {
        Self { width: 1, height: 1, values: vec![value] }
    }

    /// A 2x2 Bayer block from levels in dcraw's order: red, green on red rows,
    /// green on blue rows, blue (Sony, Olympus and Panasonic tags use it too).
    pub fn bayer(pattern: &CfaPattern, levels: [f32; 4]) -> Self {
        let mut values = Vec::with_capacity(4);
        for y in 0..2 {
            let red_row = (0..2).any(|x| pattern.color_at(x, y) == RED);
            for x in 0..2 {
                values.push(match pattern.color_at(x, y) {
                    RED => levels[0],
                    BLUE => levels[3],
                    _ if red_row => levels[1],
                    _ => levels[2],
                });
            }
        }
        Self { width: 2, height: 2, values }
    }

    pub fn at(&self, x: usize, y: usize) -> f32 {
        self.values[(y % self.height) * self.width + x % self.width]
    }
}

/// An undeveloped camera image, as a maker decoder found it in the file.
#[derive(Clone, Debug)]
pub struct NativeRaw {
    /// Decoded photosites, one sample per pixel, in raw units.
    pub plane: RawPlane,
    /// CFA colours relative to the plane's origin (0, 0). None when photosites
    /// are already full colour (`plane.samples_per_pixel == 3`, e.g. Canon sRAW).
    pub pattern: Option<CfaPattern>,
    /// The part of the plane that is image; masked borders lie outside it.
    pub visible: Area,
    pub black: BlackLevel,
    /// Saturation level in raw units, before black is subtracted.
    pub white: f32,
    /// As-shot white balance multipliers for R, G, B; None if the file has none.
    pub white_balance: Option<[f64; 3]>,
    /// Maker and model as the colour table names them, e.g. "Canon", "EOS 80D".
    pub make: String,
    pub model: String,
    /// EXIF orientation, 1..=8.
    pub orientation: u16,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demosaic::GREEN;

    #[test]
    fn bayer_levels_follow_the_pattern() {
        let pattern = CfaPattern::new(2, 2, vec![GREEN, RED, BLUE, GREEN]).unwrap();
        let black = BlackLevel::bayer(&pattern, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(black.values, [2.0, 1.0, 4.0, 3.0]);
    }
}
