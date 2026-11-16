//! Module: mirante_edit::geometry
//! Purpose: Cropping and area-averaged downscaling.
//! Created: 2026-11-16
//! Architecture: Runs on linear working pixels, where averaging is physically
//! correct; upscaling is left to Cairo at draw time.

use mirante_core::{Image, Rgba};

/// A rectangle in source-image pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CropRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl CropRect {
    /// Clips the rectangle to an image; None when nothing is left.
    pub fn clamped(self, width: u32, height: u32) -> Option<Self> {
        let x = self.x.min(width);
        let y = self.y.min(height);
        let rect =
            Self { x, y, width: self.width.min(width - x), height: self.height.min(height - y) };
        (rect.width > 0 && rect.height > 0).then_some(rect)
    }
}

pub fn crop(image: &Image, rect: CropRect) -> Image {
    let Some(rect) = rect.clamped(image.width(), image.height()) else {
        return image.clone();
    };
    let columns = rect.x as usize..(rect.x + rect.width) as usize;
    let pixels = (rect.y..rect.y + rect.height)
        .flat_map(|y| image.row(y)[columns.clone()].iter().copied())
        .collect();
    Image::new(rect.width, rect.height, pixels, image.color_space().clone())
        .expect("clamped crop keeps a non-empty, consistent size")
}

/// Shrinks to fit inside the box by averaging source pixels; never enlarges.
pub fn downscale_to_fit(image: &Image, max_width: u32, max_height: u32) -> Image {
    let scale = (f64::from(max_width) / f64::from(image.width()))
        .min(f64::from(max_height) / f64::from(image.height()));

    if scale >= 1.0 {
        return image.clone();
    }
    let width = ((f64::from(image.width()) * scale).round() as u32).max(1);
    let height = ((f64::from(image.height()) * scale).round() as u32).max(1);
    let mut pixels = Vec::with_capacity(width as usize * height as usize);

    for y in 0..height {
        let rows = source_span(y, height, image.height());
        for x in 0..width {
            let columns = source_span(x, width, image.width());
            pixels.push(average(image, rows.clone(), columns));
        }
    }
    Image::new(width, height, pixels, image.color_space().clone())
        .expect("downscaled size is non-empty and matches the pixel count")
}

/// The source rows (or columns) that land in one destination row.
fn source_span(index: u32, destination: u32, source: u32) -> std::ops::Range<u32> {
    let start = u64::from(index) * u64::from(source) / u64::from(destination);
    let end = u64::from(index + 1) * u64::from(source) / u64::from(destination);
    start as u32..(end as u32).max(start as u32 + 1)
}

fn average(image: &Image, rows: std::ops::Range<u32>, columns: std::ops::Range<u32>) -> Rgba {
    let mut sum = [0.0_f32; 4];
    let mut count = 0.0;

    for y in rows {
        for pixel in &image.row(y)[columns.start as usize..columns.end as usize] {
            // Weight colour by alpha so transparent pixels do not darken edges.
            for channel in 0..3 {
                sum[channel] += pixel[channel] * pixel[3];
            }
            sum[3] += pixel[3];
            count += 1.0;
        }
    }
    if sum[3] == 0.0 {
        return [0.0; 4];
    }
    [sum[0] / sum[3], sum[1] / sum[3], sum[2] / sum[3], sum[3] / count]
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante_core::ColorSpace;

    fn gradient(width: u32, height: u32) -> Image {
        let pixels = (0..width * height).map(|i| [i as f32, 0.0, 0.0, 1.0]).collect();
        Image::new(width, height, pixels, ColorSpace::LinearRec2020).unwrap()
    }

    #[test]
    fn crop_keeps_the_selected_pixels() {
        let cropped = crop(&gradient(4, 4), CropRect { x: 1, y: 2, width: 2, height: 1 });
        assert_eq!((cropped.width(), cropped.height()), (2, 1));
        assert_eq!(cropped.pixels()[0][0], 9.0);
    }

    #[test]
    fn crop_outside_the_image_is_clipped() {
        let rect = CropRect { x: 3, y: 3, width: 10, height: 10 };
        assert_eq!(rect.clamped(4, 4).map(|r| (r.width, r.height)), Some((1, 1)));
    }

    #[test]
    fn downscale_preserves_aspect_and_averages() {
        let small = downscale_to_fit(&gradient(4, 2), 2, 2);
        assert_eq!((small.width(), small.height()), (2, 1));
        assert_eq!(small.pixels()[0][0], 2.5);
    }
}
