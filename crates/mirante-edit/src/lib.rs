//! Module: mirante_edit
//! Purpose: The non-destructive edit list and the operations that apply it.
//! Created: 2026-11-16
//! Architecture: The original working image is never modified. Previews and
//! exports both call `Edits::apply`, so what you see is what gets saved. The
//! crop is in displayed pixels, so it follows rotations and flips.

mod adjust;
mod geometry;
mod orientation;
mod text;

pub use adjust::Adjustments;
pub use geometry::{CropRect, crop, downscale_to_fit};
pub use orientation::Orientation;

use mirante_core::Image;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Edits {
    /// In displayed pixels, after the orientation.
    pub crop: Option<CropRect>,
    pub orientation: Orientation,
    pub adjustments: Adjustments,
}

impl Edits {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Size of the result for a `source` sized image, before any downscaling.
    pub fn output_size(&self, source: (u32, u32)) -> (u32, u32) {
        let displayed = self.orientation.displayed_size(source);
        let crop = self.crop.and_then(|rect| rect.clamped(displayed.0, displayed.1));
        crop.map_or(displayed, |rect| (rect.width, rect.height))
    }

    /// Changes the orientation, moving the crop so it keeps the same content.
    pub fn reorient(&mut self, change: fn(Orientation) -> Orientation, source: (u32, u32)) {
        let after = change(self.orientation);
        let displayed = self.orientation.displayed_size(source);
        if let Some(rect) = self.crop.and_then(|rect| rect.clamped(displayed.0, displayed.1)) {
            self.crop = Some(orientation::move_rect(rect, self.orientation, after, source));
        }
        self.orientation = after;
    }

    /// Crops, shrinks to `max_size` when given, turns, then corrects colour.
    /// Cropping and shrinking in source pixels first keeps previews fast.
    pub fn apply(&self, source: &Image, max_size: Option<(u32, u32)>) -> Image {
        let size = (source.width(), source.height());
        let displayed = self.orientation.displayed_size(size);
        let in_source = self
            .crop
            .and_then(|rect| rect.clamped(displayed.0, displayed.1))
            .map(|rect| self.orientation.to_source_rect(rect, size));
        let cropped = in_source.map(|rect| crop(source, rect));
        let base = cropped.as_ref().unwrap_or(source);
        let mut image = match max_size {
            Some(max) => {
                let (width, height) =
                    if self.orientation.swaps_axes() { (max.1, max.0) } else { max };
                downscale_to_fit(base, width, height)
            }
            None => base.clone(),
        };
        if self.orientation != Orientation::default() {
            image = image.oriented(self.orientation.exif());
        }
        self.adjustments.apply(&mut image);
        image
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante_core::ColorSpace;

    fn numbered(width: u32, height: u32) -> Image {
        let pixels = (0..width * height).map(|i| [i as f32, 0.0, 0.0, 1.0]).collect();
        Image::new(width, height, pixels, ColorSpace::LinearRec2020).unwrap()
    }

    #[test]
    fn crop_applies_to_the_turned_image() {
        let source = numbered(4, 3);
        let edits = Edits {
            orientation: Orientation::default().rotated_clockwise(),
            crop: Some(CropRect { x: 1, y: 0, width: 2, height: 2 }),
            ..Default::default()
        };
        let expected =
            crop(&source.clone().oriented(6), CropRect { x: 1, y: 0, width: 2, height: 2 });
        assert_eq!(edits.apply(&source, None).pixels(), expected.pixels());
        assert_eq!(edits.output_size((4, 3)), (2, 2));
    }

    #[test]
    fn rotating_keeps_the_cropped_content() {
        let source = numbered(4, 3);
        let mut edits = Edits {
            crop: Some(CropRect { x: 0, y: 0, width: 2, height: 1 }),
            ..Default::default()
        };
        let before = edits.apply(&source, None);
        edits.reorient(Orientation::rotated_clockwise, (4, 3));
        let after = edits.apply(&source, None);
        let mut values: Vec<f32> = after.pixels().iter().map(|p| p[0]).collect();
        values.sort_by(f32::total_cmp);
        assert_eq!(values, before.pixels().iter().map(|p| p[0]).collect::<Vec<_>>());
        assert_eq!((after.width(), after.height()), (1, 2));
    }
}
