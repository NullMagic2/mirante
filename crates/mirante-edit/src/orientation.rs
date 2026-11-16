//! Module: mirante_edit::orientation
//! Purpose: Quarter-turn rotations and mirror flips as one small value.
//! Created: 2026-11-16
//! Architecture: Any mix of rotations and flips is a mirror (or not) followed by
//! 0–3 clockwise quarter turns, which maps onto an EXIF orientation that
//! Image::oriented applies. Flips act on the image as currently displayed.

use crate::CropRect;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Orientation {
    /// Clockwise quarter turns, 0..=3, applied after the mirror.
    pub quarter_turns: u8,
    /// Mirrored left to right before turning.
    pub mirrored: bool,
}

impl Orientation {
    pub fn rotated_clockwise(self) -> Self {
        Self { quarter_turns: (self.quarter_turns + 1) % 4, ..self }
    }

    pub fn rotated_counter_clockwise(self) -> Self {
        Self { quarter_turns: (self.quarter_turns + 3) % 4, ..self }
    }

    /// Mirrors left to right as displayed: H·R(t)·M(m) = R(-t)·M(m+1).
    pub fn flipped_horizontally(self) -> Self {
        Self { quarter_turns: (4 - self.quarter_turns) % 4, mirrored: !self.mirrored }
    }

    /// Mirrors top to bottom as displayed: a horizontal flip and a half turn.
    pub fn flipped_vertically(self) -> Self {
        let flipped = self.flipped_horizontally();
        Self { quarter_turns: (flipped.quarter_turns + 2) % 4, ..flipped }
    }

    pub fn swaps_axes(self) -> bool {
        self.quarter_turns % 2 == 1
    }

    /// Size of a `(width, height)` source as displayed.
    pub fn displayed_size(self, (width, height): (u32, u32)) -> (u32, u32) {
        if self.swaps_axes() { (height, width) } else { (width, height) }
    }

    /// The EXIF orientation (1..=8) with the same effect.
    pub fn exif(self) -> u16 {
        const PLAIN: [u16; 4] = [1, 6, 3, 8];
        const MIRRORED: [u16; 4] = [2, 7, 4, 5];
        let table = if self.mirrored { MIRRORED } else { PLAIN };
        table[usize::from(self.quarter_turns % 4)]
    }

    /// The source pixel shown at displayed pixel `(x, y)`, as Image::oriented maps it.
    fn source_pixel(self, (x, y): (u32, u32), (width, height): (u32, u32)) -> (u32, u32) {
        let (w, h) = (width - 1, height - 1);
        match self.exif() {
            2 => (w - x, y),
            3 => (w - x, h - y),
            4 => (x, h - y),
            5 => (y, x),
            6 => (y, h - x),
            7 => (w - y, h - x),
            8 => (w - y, x),
            _ => (x, y),
        }
    }

    /// A rectangle in displayed pixels, as source pixels of a `source` sized image.
    pub fn to_source_rect(self, rect: CropRect, source: (u32, u32)) -> CropRect {
        let last = (rect.x + rect.width.max(1) - 1, rect.y + rect.height.max(1) - 1);
        let (ax, ay) = self.source_pixel((rect.x, rect.y), source);
        let (bx, by) = self.source_pixel(last, source);
        CropRect {
            x: ax.min(bx),
            y: ay.min(by),
            width: ax.abs_diff(bx) + 1,
            height: ay.abs_diff(by) + 1,
        }
    }
}

/// Where a displayed rectangle ends up after a change of orientation, for a
/// `source` sized image. Keeps a crop on the same part of the picture.
pub fn move_rect(
    rect: CropRect,
    before: Orientation,
    after: Orientation,
    source: (u32, u32),
) -> CropRect {
    let in_source = before.to_source_rect(rect, source);
    // Mapping back out: the displayed rect is the image of the source rect.
    let corners = [
        (in_source.x, in_source.y),
        (in_source.x + in_source.width - 1, in_source.y + in_source.height - 1),
    ];
    let shown = corners.map(|corner| displayed_pixel(after, corner, source));
    let (a, b) = (shown[0], shown[1]);
    CropRect {
        x: a.0.min(b.0),
        y: a.1.min(b.1),
        width: a.0.abs_diff(b.0) + 1,
        height: a.1.abs_diff(b.1) + 1,
    }
}

/// Inverse of `source_pixel`: where a source pixel is displayed.
fn displayed_pixel(
    orientation: Orientation,
    (sx, sy): (u32, u32),
    (width, height): (u32, u32),
) -> (u32, u32) {
    let (w, h) = (width - 1, height - 1);
    match orientation.exif() {
        2 => (w - sx, sy),
        3 => (w - sx, h - sy),
        4 => (sx, h - sy),
        5 => (sy, sx),
        6 => (h - sy, sx),
        7 => (h - sy, w - sx),
        8 => (sy, w - sx),
        _ => (sx, sy),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante_core::{ColorSpace, Image};

    const SOURCE: (u32, u32) = (4, 2);

    fn numbered() -> Image {
        let pixels = (0..8).map(|i| [i as f32, 0.0, 0.0, 1.0]).collect();
        Image::new(SOURCE.0, SOURCE.1, pixels, ColorSpace::Srgb).unwrap()
    }

    #[test]
    fn four_turns_and_two_flips_cancel_out() {
        let start = Orientation::default();
        let turned = (0..4).fold(start, |o, _| o.rotated_clockwise());
        assert_eq!(turned, start);
        assert_eq!(start.flipped_horizontally().flipped_horizontally(), start);
        assert_eq!(start.rotated_clockwise().rotated_counter_clockwise(), start);
    }

    #[test]
    fn flips_follow_the_display_after_rotation() {
        // Rotating then flipping vertically equals flipping horizontally then rotating.
        let a = Orientation::default().rotated_clockwise().flipped_vertically();
        let b = Orientation::default().flipped_horizontally().rotated_clockwise();
        assert_eq!(a, b);
    }

    #[test]
    fn source_pixels_match_image_oriented() {
        for turns in 0..4 {
            for mirrored in [false, true] {
                let o = Orientation { quarter_turns: turns, mirrored };
                let shown = numbered().oriented(o.exif());
                let (w, h) = o.displayed_size(SOURCE);
                for y in 0..h {
                    for x in 0..w {
                        let (sx, sy) = o.source_pixel((x, y), SOURCE);
                        let expected = (sy * SOURCE.0 + sx) as f32;
                        assert_eq!(shown.pixels()[(y * w + x) as usize][0], expected);
                        assert_eq!(displayed_pixel(o, (sx, sy), SOURCE), (x, y));
                    }
                }
            }
        }
    }

    #[test]
    fn crops_follow_the_picture_when_rotating() {
        let crop = CropRect { x: 0, y: 0, width: 1, height: 2 };
        let before = Orientation::default();
        let moved = move_rect(crop, before, before.rotated_clockwise(), SOURCE);
        assert_eq!(moved, CropRect { x: 0, y: 0, width: 2, height: 1 });
    }
}
