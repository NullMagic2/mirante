//! Module: mirante_format_heic::transform
//! Purpose: Applies HEIF transformative properties: clean aperture, rotation, mirror.
//! Created: 2026-11-16
//! Architecture: Rotation and mirroring reuse Image::oriented through the equivalent
//! EXIF orientation; HEIF applies the properties in their association order.

use crate::properties::{CleanAperture, Property};
use mirante_core::{DecodeError, Image};

/// EXIF orientations that undo a stored anticlockwise quarter-turn count.
const ROTATION_ORIENTATIONS: [u16; 4] = [1, 8, 3, 6];
const MIRROR_LEFT_RIGHT: u16 = 2;
const MIRROR_TOP_BOTTOM: u16 = 4;

/// Applies one property if it is transformative; other properties pass through.
pub fn apply(image: Image, property: &Property) -> Result<Image, DecodeError> {
    Ok(match property {
        Property::Rotation(turns) => image.oriented(ROTATION_ORIENTATIONS[usize::from(turns & 3)]),
        Property::Mirror { top_to_bottom: true } => image.oriented(MIRROR_TOP_BOTTOM),
        Property::Mirror { top_to_bottom: false } => image.oriented(MIRROR_LEFT_RIGHT),
        Property::CleanAperture(clap) => clean_aperture(image, clap)?,
        _ => image,
    })
}

/// Crops to the clean aperture, whose centre is offset from the image centre.
fn clean_aperture(image: Image, clap: &CleanAperture) -> Result<Image, DecodeError> {
    let (width, height) = (f64::from(image.width()), f64::from(image.height()));
    let span = |size: f64, clean: f64, offset: f64| -> Option<(u32, u32)> {
        let start = (offset + (size - 1.0) / 2.0 - (clean - 1.0) / 2.0).round().max(0.0);
        let end = (start + clean.round()).min(size);
        (end > start).then_some((start as u32, (end - start) as u32))
    };
    let horizontal = span(width, clap.width, clap.horizontal_offset);
    let vertical = span(height, clap.height, clap.vertical_offset);
    let (Some((left, crop_width)), Some((top, crop_height))) = (horizontal, vertical) else {
        return Err(DecodeError::corrupt("clean aperture lies outside the image"));
    };
    crop(image, left, top, crop_width, crop_height)
}

pub fn crop(
    image: Image,
    left: u32,
    top: u32,
    width: u32,
    height: u32,
) -> Result<Image, DecodeError> {
    if (left, top, width, height) == (0, 0, image.width(), image.height()) {
        return Ok(image);
    }
    let rows = (top..top + height).map(|y| &image.row(y)[left as usize..(left + width) as usize]);
    let pixels = rows.flatten().copied().collect();
    Ok(Image::new(width, height, pixels, image.color_space().clone())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante_core::ColorSpace;

    /// A width x height image whose red channel numbers the pixels.
    fn numbered(width: u32, height: u32) -> Image {
        let pixels = (0..width * height).map(|i| [i as f32, 0.0, 0.0, 1.0]).collect();
        Image::new(width, height, pixels, ColorSpace::Srgb).unwrap()
    }

    fn reds(image: &Image) -> Vec<f32> {
        image.pixels().iter().map(|p| p[0]).collect()
    }

    #[test]
    fn rotates_anticlockwise() {
        // 0 1 2      2 5
        // 3 4 5  ->  1 4
        //            0 3
        let image = apply(numbered(3, 2), &Property::Rotation(1)).unwrap();
        assert_eq!((image.width(), image.height()), (2, 3));
        assert_eq!(reds(&image), [2.0, 5.0, 1.0, 4.0, 0.0, 3.0]);
    }

    #[test]
    fn mirrors_along_both_axes() {
        let flipped = apply(numbered(2, 2), &Property::Mirror { top_to_bottom: true }).unwrap();
        assert_eq!(reds(&flipped), [2.0, 3.0, 0.0, 1.0]);
        let flipped = apply(numbered(2, 2), &Property::Mirror { top_to_bottom: false }).unwrap();
        assert_eq!(reds(&flipped), [1.0, 0.0, 3.0, 2.0]);
    }

    #[test]
    fn crops_clean_aperture_around_offset_centre() {
        let clap =
            CleanAperture { width: 2.0, height: 1.0, horizontal_offset: 0.5, vertical_offset: 0.0 };
        let image = apply(numbered(4, 3), &Property::CleanAperture(clap)).unwrap();
        assert_eq!(reds(&image), [6.0, 7.0]);
        let outside = CleanAperture { horizontal_offset: 10.0, ..clap };
        assert!(apply(numbered(4, 3), &Property::CleanAperture(outside)).is_err());
    }
}
