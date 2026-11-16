//! Module: mirante_raw_canon::assemble
//! Purpose: Turns decoded Canon photosites and maker-note facts into a
//! NativeRaw: visible area, CFA, black and white levels, white balance.
//! Created: 2026-11-16
//! Architecture: Shared by cr2.rs and cr3.rs. The camera's own per-shot black
//! (ColorData) wins; otherwise black is measured in the masked left border as
//! dcraw's crop_masked_pixels does (ported from dcraw by Dave Coffin).

use crate::maker_note::MakerNote;
use mirante_raw::demosaic::CfaPattern;
use mirante_raw::unpack::RawPlane;
use mirante_raw::{Area, BlackLevel, NativeRaw};

pub const MAKE: &str = "Canon";
/// dcraw skips two columns at each side of the masked strip.
const MASK_MARGIN: usize = 2;
const MIN_MASK_COLUMNS: usize = 4;

/// What the container says about the camera, outside the photosites.
pub struct Camera {
    pub model: String,
    pub orientation: u16,
    pub note: MakerNote,
}

pub fn bayer(plane: RawPlane, pattern: CfaPattern, camera: Camera) -> NativeRaw {
    let full = Area { left: 0, top: 0, width: plane.width, height: plane.height };
    let sensor = camera.note.sensor.and_then(|info| info.visible(plane.width, plane.height));
    let visible = sensor.unwrap_or(full);
    let color = camera.note.color.unwrap_or_default();
    let black = match color.black {
        Some([red, green_red, green_blue, blue]) => {
            let levels = [red, green_red, green_blue, blue].map(f32::from);
            BlackLevel::bayer(&pattern, levels)
        }
        None => masked_black(&plane, visible).unwrap_or_else(|| BlackLevel::uniform(0.0)),
    };
    NativeRaw {
        plane,
        pattern: Some(pattern),
        visible,
        black,
        white: color.specular_white.map_or(0.0, f32::from),
        white_balance: color.as_shot.map(|[r, g, _, b]| [r, g, b].map(f64::from)),
        make: MAKE.to_owned(),
        model: camera.model,
        orientation: camera.orientation,
    }
}

/// Mean of each 2x2 position over the masked columns left of the image, in the
/// image's rows; None when there is no usable strip.
pub fn masked_black(plane: &RawPlane, visible: Area) -> Option<BlackLevel> {
    let (start, end) = (MASK_MARGIN, visible.left.checked_sub(MASK_MARGIN)?);
    if end < start + MIN_MASK_COLUMNS || plane.samples_per_pixel != 1 {
        return None;
    }
    let mut sums = [0_u64; 4];
    let mut counts = [0_u64; 4];
    let mut zeros = 0;
    for y in visible.top..visible.top + visible.height {
        let row = &plane.samples[y * plane.width..][..plane.width];
        for (x, &value) in row.iter().enumerate().take(end).skip(start) {
            let cell = (y & 1) * 2 + (x & 1);
            sums[cell] += u64::from(value);
            counts[cell] += 1;
            zeros += u64::from(value == 0);
        }
    }
    // A strip of zeros means the camera blanked it; dcraw then keeps black at 0.
    if counts.contains(&0) || zeros >= counts[0] {
        return None;
    }
    let values = (0..4).map(|cell| (sums[cell] / counts[cell]) as f32).collect();
    Some(BlackLevel { width: 2, height: 2, values })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante_raw::demosaic::{BLUE, GREEN, RED};

    fn plane(width: usize, height: usize, value: impl Fn(usize, usize) -> u16) -> RawPlane {
        let samples = (0..width * height).map(|i| value(i % width, i / width)).collect();
        RawPlane { width, height, samples_per_pixel: 1, bits_per_sample: 14, samples }
    }

    #[test]
    fn measures_black_per_cfa_position() {
        let raw =
            plane(
                20,
                6,
                |x, y| if x < 10 { 500 + (x & 1) as u16 + 2 * (y & 1) as u16 } else { 3000 },
            );
        let visible = Area { left: 10, top: 2, width: 10, height: 4 };
        let black = masked_black(&raw, visible).unwrap();
        assert_eq!(black.values, [500.0, 501.0, 502.0, 503.0]);
        assert!(masked_black(&raw, Area { left: 4, ..visible }).is_none());
    }

    #[test]
    fn prefers_the_camera_levels() {
        let raw = plane(8, 8, |_, _| 600);
        let pattern = CfaPattern::new(2, 2, vec![RED, GREEN, GREEN, BLUE]).unwrap();
        let color = crate::maker_note::ColorData {
            as_shot: Some([2000, 1024, 1024, 1500]),
            black: Some([510, 511, 512, 513]),
            specular_white: Some(15000),
            sraw_mul: None,
        };
        let note = MakerNote { color: Some(color), ..MakerNote::default() };
        let camera = Camera { model: "EOS R6".into(), orientation: 1, note };
        let native = bayer(raw, pattern, camera);
        assert_eq!(native.black.values, [510.0, 511.0, 512.0, 513.0]);
        assert_eq!((native.white, native.white_balance), (15000.0, Some([2000.0, 1024.0, 1500.0])));
        assert_eq!(native.visible, Area { left: 0, top: 0, width: 8, height: 8 });
    }
}
