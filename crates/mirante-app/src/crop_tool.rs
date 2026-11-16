//! Module: mirante::crop_tool
//! Purpose: The crop selection's geometry: what a pointer grabs, how a drag
//! moves or resizes the selection, and which cursor to show.
//! Created: 2026-11-16
//! Architecture: Pure maths in source-image pixels, so it is testable without
//! GTK; viewer.rs converts pointer positions and draws the result.

use mirante_edit::CropRect;

/// Grab distance around edges and handles, in screen pixels.
pub const HANDLE_REACH: f64 = 10.0;
/// The selection never shrinks below this, in source pixels.
const MIN_SIZE: f64 = 8.0;

/// A selection in source pixels; fractional while dragging.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn whole((width, height): (u32, u32)) -> Self {
        Self { x: 0.0, y: 0.0, width: f64::from(width), height: f64::from(height) }
    }

    pub fn from_crop(crop: CropRect) -> Self {
        Self {
            x: f64::from(crop.x),
            y: f64::from(crop.y),
            width: f64::from(crop.width),
            height: f64::from(crop.height),
        }
    }

    /// Whole pixels; None when the selection is the entire image (no crop).
    pub fn to_crop(self, (width, height): (u32, u32)) -> Option<CropRect> {
        let x = self.x.round().max(0.0) as u32;
        let y = self.y.round().max(0.0) as u32;
        let right = ((self.x + self.width).round() as u32).min(width);
        let bottom = ((self.y + self.height).round() as u32).min(height);
        let crop =
            CropRect { x, y, width: right.saturating_sub(x), height: bottom.saturating_sub(y) };
        let whole = crop == CropRect { x: 0, y: 0, width, height };
        (!whole && crop.width > 0 && crop.height > 0).then_some(crop)
    }

    fn right(&self) -> f64 {
        self.x + self.width
    }

    fn bottom(&self) -> f64 {
        self.y + self.height
    }

    fn is_tiny(&self) -> bool {
        self.width < MIN_SIZE || self.height < MIN_SIZE
    }
}

/// Which sides a resize moves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Edges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grip {
    Resize(Edges),
    Move,
    /// Outside the selection: drawing a new one.
    New,
}

/// What a press at `point` takes hold of; `reach` is HANDLE_REACH in source pixels.
pub fn grip(rect: Rect, (x, y): (f64, f64), reach: f64) -> Grip {
    let along_x = x >= rect.x - reach && x <= rect.right() + reach;
    let along_y = y >= rect.y - reach && y <= rect.bottom() + reach;
    // When the selection is narrower than two reaches, the nearer side wins.
    let closer = |a: f64, b: f64, v: f64| (v - a).abs() <= (v - b).abs();
    let near = |edge: f64, v: f64| (v - edge).abs() <= reach;
    let left = along_y && near(rect.x, x) && closer(rect.x, rect.right(), x);
    let right = along_y && near(rect.right(), x) && !left;
    let top = along_x && near(rect.y, y) && closer(rect.y, rect.bottom(), y);
    let bottom = along_x && near(rect.bottom(), y) && !top;
    let edges = Edges { left, right, top, bottom };

    if edges != Edges::default() {
        Grip::Resize(edges)
    } else if x > rect.x && x < rect.right() && y > rect.y && y < rect.bottom() {
        Grip::Move
    } else {
        Grip::New
    }
}

/// The selection after dragging `grip` of `original` from `start` to `now`,
/// kept inside an image of `bounds` and no smaller than MIN_SIZE.
pub fn drag(
    grip: Grip,
    original: Rect,
    start: (f64, f64),
    now: (f64, f64),
    bounds: (u32, u32),
) -> Rect {
    let (width, height) = (f64::from(bounds.0), f64::from(bounds.1));
    let (dx, dy) = (now.0 - start.0, now.1 - start.1);
    match grip {
        Grip::Move => Rect {
            x: (original.x + dx).clamp(0.0, (width - original.width).max(0.0)),
            y: (original.y + dy).clamp(0.0, (height - original.height).max(0.0)),
            ..original
        },
        Grip::Resize(edges) => {
            let (mut left, mut right) = (original.x, original.right());
            let (mut top, mut bottom) = (original.y, original.bottom());
            if edges.left {
                left = (left + dx).clamp(0.0, (right - MIN_SIZE).max(0.0));
            }
            if edges.right {
                right = (right + dx).clamp((left + MIN_SIZE).min(width), width);
            }
            if edges.top {
                top = (top + dy).clamp(0.0, (bottom - MIN_SIZE).max(0.0));
            }
            if edges.bottom {
                bottom = (bottom + dy).clamp((top + MIN_SIZE).min(height), height);
            }
            Rect { x: left, y: top, width: right - left, height: bottom - top }
        }
        Grip::New => {
            let clamp = |(x, y): (f64, f64)| (x.clamp(0.0, width), y.clamp(0.0, height));
            let ((x0, y0), (x1, y1)) = (clamp(start), clamp(now));
            Rect { x: x0.min(x1), y: y0.min(y1), width: (x1 - x0).abs(), height: (y1 - y0).abs() }
        }
    }
}

/// Keeps a freshly drawn selection only if it is big enough to mean something.
pub fn finish(grip: Grip, original: Rect, result: Rect) -> Rect {
    if grip == Grip::New && result.is_tiny() { original } else { result }
}

/// The standard cursor name for hovering over `grip`.
pub fn cursor_name(grip: Grip) -> &'static str {
    match grip {
        Grip::Move => "move",
        Grip::New => "crosshair",
        Grip::Resize(e) => match (e.left || e.right, e.top || e.bottom) {
            (true, true) if e.left == e.top => "nwse-resize",
            (true, true) => "nesw-resize",
            (true, false) => "ew-resize",
            _ => "ns-resize",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: (u32, u32) = (100, 80);
    const RECT: Rect = Rect { x: 20.0, y: 10.0, width: 40.0, height: 30.0 };

    #[test]
    fn grips_corners_edges_inside_and_outside() {
        let corner = Edges { left: true, top: true, ..Default::default() };
        assert_eq!(grip(RECT, (21.0, 11.0), 3.0), Grip::Resize(corner));
        let right = Edges { right: true, ..Default::default() };
        assert_eq!(grip(RECT, (59.0, 25.0), 3.0), Grip::Resize(right));
        assert_eq!(grip(RECT, (40.0, 25.0), 3.0), Grip::Move);
        assert_eq!(grip(RECT, (90.0, 70.0), 3.0), Grip::New);
    }

    #[test]
    fn resizing_stays_inside_and_above_the_minimum() {
        let left = Grip::Resize(Edges { left: true, ..Default::default() });
        let grown = drag(left, RECT, (20.0, 20.0), (-50.0, 20.0), BOUNDS);
        assert_eq!((grown.x, grown.width), (0.0, 60.0));
        let squashed = drag(left, RECT, (20.0, 20.0), (90.0, 20.0), BOUNDS);
        assert_eq!(squashed.width, MIN_SIZE);
    }

    #[test]
    fn moving_keeps_size_and_stays_inside() {
        let moved = drag(Grip::Move, RECT, (30.0, 20.0), (200.0, -50.0), BOUNDS);
        assert_eq!(moved, Rect { x: 60.0, y: 0.0, ..RECT });
    }

    #[test]
    fn whole_image_means_no_crop_and_stray_clicks_are_ignored() {
        assert_eq!(Rect::whole(BOUNDS).to_crop(BOUNDS), None);
        let crop = RECT.to_crop(BOUNDS).unwrap();
        assert_eq!((crop.x, crop.y, crop.width, crop.height), (20, 10, 40, 30));
        let click = drag(Grip::New, RECT, (5.0, 5.0), (6.0, 6.0), BOUNDS);
        assert_eq!(finish(Grip::New, RECT, click), RECT);
    }

    #[test]
    fn cursors_match_the_grip() {
        let corner = |left, top| Edges { left, right: !left, top, bottom: !top };
        assert_eq!(cursor_name(Grip::Resize(corner(true, true))), "nwse-resize");
        assert_eq!(cursor_name(Grip::Resize(corner(false, true))), "nesw-resize");
        assert_eq!(cursor_name(Grip::Move), "move");
    }
}
