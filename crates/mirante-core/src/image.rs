//! Module: mirante_core::image
//! Purpose: An RGBA float image tagged with the colour space its values are in.
//! Created: 2026-11-16
//! Architecture: Decoders produce it in the file's own space; mirante-color moves
//! it to the working space. Alpha is straight, never premultiplied.

use std::{fmt, sync::Arc};

/// One pixel: red, green, blue, alpha. Colour may exceed 0..1 in linear spaces.
pub type Rgba = [f32; 4];

#[derive(Clone, Debug, PartialEq)]
pub enum ColorSpace {
    /// sRGB with its transfer curve, the assumption for untagged files.
    Srgb,
    LinearSrgb,
    /// The editing space: Rec. 2020 primaries, D65, linear.
    LinearRec2020,
    /// An embedded ICC profile, kept verbatim.
    Icc(Arc<[u8]>),
}

#[derive(Clone, Debug)]
pub struct Image {
    width: u32,
    height: u32,
    pixels: Vec<Rgba>,
    color_space: ColorSpace,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ImageError {
    Empty,
    SizeMismatch { expected: usize, actual: usize },
}

impl fmt::Display for ImageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(formatter, "the image has no pixels"),
            Self::SizeMismatch { expected, actual } => {
                write!(formatter, "expected {expected} pixels, got {actual}")
            }
        }
    }
}

impl std::error::Error for ImageError {}

impl Image {
    pub fn new(
        width: u32,
        height: u32,
        pixels: Vec<Rgba>,
        color_space: ColorSpace,
    ) -> Result<Self, ImageError> {
        let expected = width as usize * height as usize;

        if expected == 0 {
            return Err(ImageError::Empty);
        }
        if pixels.len() != expected {
            return Err(ImageError::SizeMismatch { expected, actual: pixels.len() });
        }
        Ok(Self { width, height, pixels, color_space })
    }

    /// Builds an image from interleaved 8-bit RGBA bytes.
    pub fn from_rgba8(
        width: u32,
        height: u32,
        bytes: &[u8],
        color_space: ColorSpace,
    ) -> Result<Self, ImageError> {
        const MAX: f32 = u8::MAX as f32;
        let pixels = bytes
            .chunks_exact(4)
            .map(|p| [p[0], p[1], p[2], p[3]].map(|v| f32::from(v) / MAX))
            .collect();
        Self::new(width, height, pixels, color_space)
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn pixels(&self) -> &[Rgba] {
        &self.pixels
    }

    pub fn pixels_mut(&mut self) -> &mut [Rgba] {
        &mut self.pixels
    }

    pub fn color_space(&self) -> &ColorSpace {
        &self.color_space
    }

    /// Relabels the pixels after a colour transform has rewritten them.
    pub fn with_color_space(self, color_space: ColorSpace) -> Self {
        Self { color_space, ..self }
    }

    /// Applies an EXIF/TIFF orientation (1..=8) so the image displays upright.
    pub fn oriented(self, orientation: u16) -> Self {
        let (w, h) = (self.width as usize, self.height as usize);
        let swaps = matches!(orientation, 5..=8);
        let (out_w, out_h) = if swaps { (h, w) } else { (w, h) };
        if !(2..=8).contains(&orientation) {
            return self;
        }
        let mut pixels = Vec::with_capacity(self.pixels.len());

        for y in 0..out_h {
            for x in 0..out_w {
                // Where the displayed pixel (x, y) comes from in the stored image.
                let (sx, sy) = match orientation {
                    2 => (w - 1 - x, y),
                    3 => (w - 1 - x, h - 1 - y),
                    4 => (x, h - 1 - y),
                    5 => (y, x),
                    6 => (y, h - 1 - x),
                    7 => (w - 1 - y, h - 1 - x),
                    _ => (w - 1 - y, x),
                };
                pixels.push(self.pixels[sy * w + sx]);
            }
        }
        Self { width: out_w as u32, height: out_h as u32, pixels, ..self }
    }

    pub fn row(&self, y: u32) -> &[Rgba] {
        let start = y as usize * self.width as usize;
        &self.pixels[start..start + self.width as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_wrong_pixel_count() {
        let result = Image::new(2, 2, vec![[0.0; 4]; 3], ColorSpace::Srgb);
        assert_eq!(result.unwrap_err(), ImageError::SizeMismatch { expected: 4, actual: 3 });
    }

    #[test]
    fn orientation_6_rotates_clockwise() {
        let pixels = vec![[1.0; 4], [2.0; 4]];
        let image = Image::new(2, 1, pixels, ColorSpace::Srgb).unwrap().oriented(6);
        assert_eq!((image.width(), image.height()), (1, 2));
        assert_eq!(image.pixels()[0][0], 1.0);
    }

    #[test]
    fn converts_rgba8() {
        let image = Image::from_rgba8(1, 1, &[255, 0, 51, 255], ColorSpace::Srgb).unwrap();
        assert_eq!(image.pixels()[0], [1.0, 0.0, 0.2, 1.0]);
    }
}
