//! Module: mirante_raw::develop
//! Purpose: Turns raw photosites into a linear Rec. 2020 Image.
//! Created: 2026-11-16
//! Architecture: normalise (levels.rs) -> white-balance gains and clip ->
//! demosaic (demosaic.rs) -> camera matrix (color.rs Rendering) -> orientation.
//! DNG (plugins/raw/src/dng.rs) shares `camera_pixels` and `finish`. Maker files
//! get their matrix and missing levels from the camera table (camera.rs), or a
//! fit against the embedded preview (fit.rs) for cameras the table lacks.

use crate::camera::{self, CameraProfile};
use crate::color::{CameraColor, Matrix, Rendering, Vector, diagonal, is_positive, multiply};
use crate::demosaic::{CfaPattern, Mosaic};
use crate::fit;
use crate::frame::{Area, BlackLevel, NativeRaw};
use crate::levels::{Levels, normalise};
use mirante_core::{ColorSpace, DecodeError, Image, Rgba, parallel};

/// Camera RGB inside `crop` from normalised `values` over `area` (CFA pattern
/// relative to `area`, else 1 or 3 samples). Gains and the clip at 1 come before
/// demosaicing, so it sees highlights clip to neutral.
pub fn camera_pixels(
    mut values: Vec<f32>,
    area: Area,
    pattern: Option<&CfaPattern>,
    crop: Area,
    gains: [f32; 3],
) -> Result<Vec<Rgba>, DecodeError> {
    let (Ok(width), Ok(height)) = (u32::try_from(area.width), u32::try_from(area.height)) else {
        return Err(DecodeError::unsupported("raw image size"));
    };
    let pixels = DecodeError::check_size(width, height)?;
    let inside = crop.width > 0
        && crop.height > 0
        && crop.left.saturating_add(crop.width) <= area.width
        && crop.top.saturating_add(crop.height) <= area.height;
    let spp = values.len() / pixels;
    if !inside || values.len() != pixels * spp {
        return Err(DecodeError::corrupt("raw crop lies outside the image area"));
    }
    parallel::for_each_indexed_chunk(&mut values, |first, part| {
        for (offset, value) in part.iter_mut().enumerate() {
            let index = first + offset;
            let color = match (pattern, spp) {
                (Some(pattern), _) => {
                    usize::from(pattern.color_at(index % area.width, index / area.width))
                }
                (None, 3) => index % 3,
                _ => continue,
            };
            *value = (*value * gains[color]).min(1.0);
        }
    });
    match (pattern, spp) {
        (Some(pattern), 1) => {
            let mosaic =
                Mosaic { width: area.width, height: area.height, values: &values, pattern };
            Ok(mosaic.demosaic(crop.left, crop.top, crop.width, crop.height))
        }
        (None, 1 | 3) => {
            let mut pixels = Vec::with_capacity(crop.width * crop.height);
            for y in crop.top..crop.top + crop.height {
                for x in crop.left..crop.left + crop.width {
                    let at = (y * area.width + x) * spp;
                    let rgb =
                        if spp == 1 { [values[at]; 3] } else { [0, 1, 2].map(|i| values[at + i]) };
                    pixels.push([rgb[0], rgb[1], rgb[2], 1.0]);
                }
            }
            Ok(pixels)
        }
        _ => Err(DecodeError::unsupported(format!("raw data with {spp} samples per pixel"))),
    }
}

/// Renders pixels from `camera_pixels` (made with `rendering.gains()`) to linear
/// Rec. 2020 and turns the image upright.
pub fn finish(
    pixels: Vec<Rgba>,
    crop: Area,
    rendering: &Rendering,
    orientation: u16,
) -> Result<Image, DecodeError> {
    let mut image =
        Image::new(crop.width as u32, crop.height as u32, pixels, ColorSpace::LinearRec2020)?;
    rendering.for_balanced().apply(image.pixels_mut());
    Ok(image.oriented(orientation))
}

/// `preview` is called only when the camera's colour matrix or white balance is
/// unknown, so they can be fitted against the camera's own rendering.
pub fn develop(raw: NativeRaw, preview: &dyn Fn() -> Option<Image>) -> Result<Image, DecodeError> {
    let visible = raw.visible;
    let pattern = match (&raw.pattern, raw.plane.samples_per_pixel) {
        (Some(pattern), 1) => Some(pattern.shifted(visible.left, visible.top)),
        (None, 3) => None,
        _ => return Err(DecodeError::corrupt("raw CFA pattern does not match its samples")),
    };
    let profile = camera::profile(&raw.make, &raw.model);
    let values = normalise(&raw.plane, visible, &levels(&raw, profile.as_ref()))?;
    let neutral = raw.white_balance.filter(is_positive).map(|m| m.map(|v| 1.0 / v));
    let matrix = profile.and_then(|p| p.xyz_to_camera);
    let rendering = match (matrix, neutral) {
        (Some(matrix), Some(neutral)) => CameraColor::d65(matrix, neutral).rendering()?,
        _ => {
            let size = (visible.width, visible.height);
            let camera = fit::camera_thumbnail(&values, size, pattern.as_ref(), raw.orientation);
            estimated(camera, &raw, matrix, neutral, preview)?
        }
    };
    let crop = Area { left: 0, top: 0, ..visible };
    let pixels = camera_pixels(values, visible, pattern.as_ref(), crop, rendering.gains())?;
    finish(pixels, crop, &rendering, raw.orientation)
}

/// The maker's levels, with the table's black and white where the decoder gave none.
fn levels(raw: &NativeRaw, profile: Option<&CameraProfile>) -> Levels {
    let unset_black = raw.black.values.iter().all(|&v| v == 0.0);
    let black = match profile.and_then(|p| p.black) {
        Some(black) if unset_black => BlackLevel::uniform(black),
        _ => raw.black.clone(),
    };
    let full_scale = ((1_u32 << raw.plane.bits_per_sample.clamp(1, 16)) - 1) as f32;
    let white = if raw.white > 0.0 {
        raw.white
    } else {
        profile.and_then(|p| p.white).unwrap_or(full_scale)
    };
    Levels::simple(black, white, raw.plane.samples_per_pixel)
}

/// Fills in what the file and table lack: the matrix, the white balance or both,
/// from the preview when there is one, else from the maker's typical matrix
/// and a grey-world white balance. `camera` is the upright camera thumbnail.
fn estimated(
    camera: Option<Image>,
    raw: &NativeRaw,
    matrix: Option<Matrix>,
    neutral: Option<Vector>,
    preview: &dyn Fn() -> Option<Image>,
) -> Result<Rendering, DecodeError> {
    // A known camera's own matrix is the best prior for its white balance fit.
    let prior = matrix
        .and_then(|m| camera::balanced_d65(&m))
        .map_or_else(|| camera::prior(&raw.make, &raw.model), |(balanced, _)| balanced);
    let fitted = camera.as_ref().and_then(|camera| {
        let samples = fit::samples(camera, &fit::preview_thumbnail(preview()?));
        fit::fit_matrix(&samples, &prior, neutral)
    });
    let (fitted_matrix, neutral) = match fitted {
        Some((fitted_matrix, neutral)) => (Some(fitted_matrix), neutral),
        None => {
            let name = format!("{} {}", raw.make, raw.model);
            eprintln!("mirante: no usable preview to calibrate {name}; colours are approximate");
            let grey = camera.as_ref().and_then(fit::grey_world);
            (None, neutral.or(grey).unwrap_or([1.0; 3]))
        }
    };
    match (matrix, fitted_matrix) {
        (Some(matrix), _) => CameraColor::d65(matrix, neutral).rendering(),
        (None, Some(fitted_matrix)) => Rendering::new(&fitted_matrix, &neutral, 0.0),
        (None, None) => {
            let unbalanced = multiply(&prior, &diagonal(&neutral.map(|v| 1.0 / v)));
            Rendering::new(&unbalanced, &neutral, 0.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demosaic::{BLUE, GREEN, RED};
    use crate::unpack::RawPlane;

    const WHITE: f32 = 4000.0;
    const AS_SHOT: [f64; 3] = [2.0, 1.0, 1.5];
    const GREY: f32 = 0.3;

    fn bayer() -> CfaPattern {
        CfaPattern::new(2, 2, vec![RED, GREEN, GREEN, BLUE]).unwrap()
    }

    fn xtrans() -> CfaPattern {
        let colors = b"GGRGGBGGBGGRBRGRBGGGBGGRGGRGGBRBGBRG"
            .iter()
            .map(|c| match c {
                b'R' => RED,
                b'G' => GREEN,
                _ => BLUE,
            })
            .collect();
        CfaPattern::new(6, 6, colors).unwrap()
    }

    /// Camera values (0..1 of white) of a grey at GREY under the as-shot light.
    fn grey(_: usize, _: usize) -> [f32; 3] {
        let neutral = AS_SHOT.map(|m| 1.0 / m);
        let largest = neutral.iter().copied().fold(0.0, f64::max);
        neutral.map(|v| GREY * (v / largest) as f32)
    }

    /// A sensor with two masked columns and one masked row, recording `scene`.
    fn native(
        width: usize,
        height: usize,
        pattern: Option<CfaPattern>,
        black: BlackLevel,
        scene: impl Fn(usize, usize) -> [f32; 3],
    ) -> NativeRaw {
        let spp = if pattern.is_some() { 1 } else { 3 };
        let mut samples = Vec::with_capacity(width * height * spp);
        for y in 0..height {
            for x in 0..width {
                let rgb = scene(x, y);
                let level = black.at(x, y);
                let mut push = |v: f32| samples.push((level + v * (WHITE - level)).round() as u16);
                match &pattern {
                    Some(pattern) => push(rgb[usize::from(pattern.color_at(x, y))]),
                    None => rgb.into_iter().for_each(push),
                }
            }
        }
        let plane =
            RawPlane { width, height, samples_per_pixel: spp, bits_per_sample: 12, samples };
        NativeRaw {
            plane,
            pattern,
            visible: Area { left: 2, top: 1, width: width - 2, height: height - 1 },
            black,
            white: WHITE,
            white_balance: Some(AS_SHOT),
            make: "SONY".to_owned(),
            model: "ILCE-7M3".to_owned(),
            orientation: 1,
        }
    }

    fn no_preview() -> Option<Image> {
        None
    }

    fn assert_grey(image: &Image, tolerance: f32) {
        for pixel in image.pixels() {
            assert!(pixel[..3].iter().all(|v| (v - GREY).abs() < tolerance), "{pixel:?}");
            assert_eq!(pixel[3], 1.0);
        }
    }

    #[test]
    fn grey_stays_grey_after_white_balance() {
        let raw = native(20, 15, Some(bayer()), BlackLevel::uniform(128.0), grey);
        let image = develop(raw, &no_preview).unwrap();
        assert_eq!((image.width(), image.height()), (18, 14));
        assert_eq!(*image.color_space(), ColorSpace::LinearRec2020);
        assert_grey(&image, 2e-3);
    }

    #[test]
    fn subtracts_a_per_colour_black_pattern() {
        let black = BlackLevel::bayer(&bayer(), [500.0, 510.0, 520.0, 530.0]);
        let raw = native(20, 15, Some(bayer()), black, grey);
        assert_grey(&develop(raw, &no_preview).unwrap(), 2e-3);
    }

    #[test]
    fn flat_colours_are_uniform_and_follow_the_camera_matrix() {
        let colour = |_: usize, _: usize| [0.6, 0.25, 0.1];
        let raw = native(20, 15, Some(bayer()), BlackLevel::uniform(0.0), colour);
        let image = develop(raw, &no_preview).unwrap();
        let matrix = camera::profile("Sony", "ILCE-7M3").unwrap().xyz_to_camera.unwrap();
        let neutral = AS_SHOT.map(|m| 1.0 / m);
        let rendering = CameraColor::d65(matrix, neutral).rendering().unwrap();
        let mut expected = [[0.6, 0.25, 0.1, 1.0]];
        rendering.apply(&mut expected);
        for pixel in image.pixels() {
            assert!(pixel.iter().zip(expected[0]).all(|(a, b)| (a - b).abs() < 2e-3), "{pixel:?}");
        }
        assert!(expected[0][0] > expected[0][1] && expected[0][1] > expected[0][2]);
    }

    #[test]
    fn highlights_clip_to_neutral() {
        let raw = native(20, 15, Some(bayer()), BlackLevel::uniform(0.0), |_, _| [1.0; 3]);
        let image = develop(raw, &no_preview).unwrap();
        assert!(image.pixels().iter().all(|p| p[..3].iter().all(|v| (v - 1.0).abs() < 2e-3)));
    }

    #[test]
    fn clipped_edges_stay_neutral_on_xtrans() {
        // Grey next to a fully saturated area: clipping before demosaicing must
        // not leave coloured fringes along the edge.
        let scene = |x: usize, y: usize| if x + y / 3 < 20 { grey(x, y) } else { [1.0; 3] };
        let mut raw = native(40, 30, Some(xtrans()), BlackLevel::uniform(0.0), scene);
        (raw.make, raw.model) = ("FUJIFILM".to_owned(), "X100F".to_owned());
        for pixel in develop(raw, &no_preview).unwrap().pixels() {
            let spread = pixel[..3].iter().fold(0.0_f32, |a, &v| a.max(v))
                - pixel[..3].iter().fold(f32::MAX, |a, &v| a.min(v));
            assert!(spread < 0.02 * pixel[1].max(GREY), "{pixel:?}");
        }
    }

    #[test]
    fn applies_orientation() {
        let mut raw = native(20, 15, Some(bayer()), BlackLevel::uniform(64.0), grey);
        raw.orientation = 6;
        let image = develop(raw, &no_preview).unwrap();
        assert_eq!((image.width(), image.height()), (14, 18));
    }

    #[test]
    fn xtrans_flat_field_stays_flat() {
        let mut raw = native(30, 25, Some(xtrans()), BlackLevel::uniform(256.0), grey);
        (raw.make, raw.model) = ("FUJIFILM".to_owned(), "X100F".to_owned());
        assert_grey(&develop(raw, &no_preview).unwrap(), 2e-3);
    }

    #[test]
    fn full_colour_photosites_skip_demosaicing() {
        let raw = native(8, 6, None, BlackLevel::uniform(100.0), grey);
        assert_grey(&develop(raw, &no_preview).unwrap(), 2e-3);
    }

    #[test]
    fn rejects_a_pattern_that_does_not_match_the_samples() {
        let mut raw = native(8, 6, None, BlackLevel::uniform(0.0), grey);
        raw.pattern = Some(bayer());
        assert!(matches!(develop(raw, &no_preview), Err(DecodeError::Corrupt(_))));
    }

    #[test]
    fn uses_table_levels_when_the_decoder_has_none() {
        // The table gives the G9 a black of 15 and a white of 4095.
        let mut raw = native(20, 15, Some(bayer()), BlackLevel::uniform(15.0), grey);
        (raw.make, raw.model) = ("Panasonic".to_owned(), "DC-G9".to_owned());
        raw.plane
            .samples
            .iter_mut()
            .for_each(|v| *v = 15 + (f32::from(*v - 15) * 4080.0 / 3985.0).round() as u16);
        (raw.black, raw.white) = (BlackLevel::uniform(0.0), 0.0);
        assert_grey(&develop(raw, &no_preview).unwrap(), 3e-3);
    }

    #[test]
    fn unknown_camera_without_preview_or_white_balance_still_develops_grey() {
        let mut raw = native(40, 30, Some(bayer()), BlackLevel::uniform(0.0), grey);
        (raw.make, raw.model, raw.white_balance) = ("Canon".to_owned(), "EOS R6".to_owned(), None);
        assert_grey(&develop(raw, &no_preview).unwrap(), 5e-3);
    }

    #[test]
    fn unknown_camera_is_calibrated_against_its_preview() {
        // Blocks of colour; the camera sees them through a matrix the table lacks.
        let truth = [[1.8, -0.6, -0.2], [-0.3, 1.5, -0.2], [0.0, -0.5, 1.5]];
        let to_camera = crate::color::invert(&truth).unwrap();
        let neutral = AS_SHOT.map(|m| 1.0 / m);
        let scene = |x: usize, y: usize| -> [f64; 3] {
            let block = (x / 12 * 7 + y / 12 * 13) % 17;
            let channel = |k: usize| 0.08 + 0.25 * (((block * (k + 3) * 5) % 11) as f64 / 10.0);
            [channel(0), channel(1), channel(2)]
        };
        let camera = move |x: usize, y: usize| {
            let balanced = crate::color::apply(&to_camera, &scene(x, y));
            [0, 1, 2].map(|i| (balanced[i] * neutral[i] / 2.0) as f32)
        };
        let (width, height) = (242, 181);
        let mut raw = native(width, height, Some(bayer()), BlackLevel::uniform(0.0), camera);
        (raw.make, raw.model) = ("Canon".to_owned(), "EOS R6".to_owned());
        let preview = || {
            let pixels = (0..(width - 2) * (height - 1))
                .map(|i| {
                    let s = scene(2 + i % (width - 2), 1 + i / (width - 2));
                    [s[0] as f32 * 0.7, s[1] as f32 * 0.7, s[2] as f32 * 0.7, 1.0]
                })
                .collect();
            Image::new((width - 2) as u32, (height - 1) as u32, pixels, ColorSpace::LinearRec2020)
                .ok()
        };
        let image = develop(raw, &preview).unwrap();
        // Compare chromaticity at block centres, away from demosaicing seams.
        for (x, y) in [(8, 8), (32, 20), (56, 44), (104, 92), (152, 140)] {
            let pixel = image.row(y as u32 - 1)[x - 2];
            let expected = scene(x, y);
            let sum: f32 = pixel[..3].iter().sum();
            let expected_sum: f64 = expected.iter().sum();
            for i in 0..3 {
                let error = pixel[i] / sum - (expected[i] / expected_sum) as f32;
                assert!(error.abs() < 0.01, "{x},{y}: {pixel:?} vs {expected:?}");
            }
        }
    }
}
