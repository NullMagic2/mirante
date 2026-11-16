//! Module: mirante_raw::color
//! Purpose: Camera colour: white balance and the camera to linear Rec. 2020 matrix.
//! Created: 2026-11-16
//! Architecture: Follows the DNG specification, chapter 6: ColorMatrix (and
//! ForwardMatrix) are interpolated by inverse colour temperature for the as-shot
//! white, give camera to XYZ D50, then Bradford to D65 and Rec. 2020. Table and
//! fitted matrices reuse it. Rendering applies the result to pixels in parallel.

use mirante_color::matrix;
use mirante_core::{DecodeError, Rgba, parallel};

pub type Matrix = [[f64; 3]; 3];
pub type Vector = [f64; 3];

pub const IDENTITY: Matrix = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
pub const D50_XY: (f64, f64) = (0.3457, 0.3585);
pub const D65_XY: (f64, f64) = (0.3127, 0.3290);
/// XYZ (D65) to linear Rec. 2020, from the BT.2020 primaries.
pub const XYZ_TO_REC2020: Matrix = [
    [1.716_651_2, -0.355_670_8, -0.253_366_3],
    [-0.666_684_4, 1.616_481_2, 0.015_768_5],
    [0.017_639_9, -0.042_770_6, 0.942_103_1],
];
/// Luminance (Y) of linear Rec. 2020 values.
pub const REC2020_LUMINANCE: Vector = [0.262_700_2, 0.677_998_1, 0.059_301_7];
/// Iterations of the as-shot neutral to white point search; it converges in a few.
const WHITE_SEARCH_STEPS: usize = 30;
const WHITE_SEARCH_TOLERANCE: f64 = 1e-7;
const MIN_TEMPERATURE: f64 = 2000.0;
const D65_TEMPERATURE: f64 = 6504.0;
const MAX_TEMPERATURE: f64 = 50_000.0;

pub use mirante_color::matrix::{diagonal, multiply, xy_to_xyz};

pub fn apply(m: &Matrix, v: &Vector) -> Vector {
    matrix::apply(m, *v)
}

pub fn invert(m: &Matrix) -> Option<Matrix> {
    matrix::inverse(m)
}

fn lerp(a: &Matrix, b: &Matrix, weight: f64) -> Matrix {
    let mut out = *b;
    for (out_row, (a_row, b_row)) in out.iter_mut().zip(a.iter().zip(b)) {
        for (value, (a, b)) in out_row.iter_mut().zip(a_row.iter().zip(b_row)) {
            *value = weight * a + (1.0 - weight) * b;
        }
    }
    out
}

fn xyz_to_xy(v: &Vector) -> Option<(f64, f64)> {
    let sum = v[0] + v[1] + v[2];
    (sum.is_normal() && v[1] > 0.0).then(|| (v[0] / sum, v[1] / sum))
}

/// Bradford chromatic adaptation between two white points given as XYZ.
pub fn bradford(source: &Vector, destination: &Vector) -> Matrix {
    matrix::adaptation(*source, *destination)
}

/// Correlated colour temperature by McCamy's approximation, clamped to a sane range.
pub fn temperature((x, y): (f64, f64)) -> f64 {
    let n = (x - 0.3320) / (0.1858 - y);
    let cct = ((449.0 * n + 3525.0) * n + 6823.3) * n + 5520.33;
    cct.clamp(MIN_TEMPERATURE, MAX_TEMPERATURE)
}

/// Colour temperature of an EXIF LightSource code, as used by CalibrationIlluminant.
pub fn illuminant_temperature(code: u32) -> Option<f64> {
    Some(match code {
        1 | 4 | 9 => 5500.0,
        2 | 14 => 4150.0,
        3 => 2850.0,
        10 => 6500.0,
        11 => 7500.0,
        12 => 6430.0,
        13 => 5000.0,
        15 => 3450.0,
        16 => 3000.0,
        17 => 2856.0,
        18 => 4874.0,
        19 => 6774.0,
        20 => 5503.0,
        21 => 6504.0,
        22 => 7504.0,
        23 => 5003.0,
        24 => 3200.0,
        _ => return None,
    })
}

/// The colour data measured under one calibration illuminant.
#[derive(Clone, Debug, PartialEq)]
pub struct Calibration {
    pub temperature: f64,
    /// XYZ to reference camera space (ColorMatrixN).
    pub color_matrix: Matrix,
    /// Reference to individual camera (CameraCalibrationN), identity if absent.
    pub camera_calibration: Matrix,
    pub forward_matrix: Option<Matrix>,
}

/// How the file states the white balance at capture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AsShot {
    Neutral(Vector),
    WhiteXy((f64, f64)),
    Unknown,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CameraColor {
    pub calibration: Calibration,
    /// The second illuminant's data, when the file has two.
    pub second_calibration: Option<Calibration>,
    pub analog_balance: Vector,
    pub as_shot: AsShot,
    pub baseline_exposure: f64,
}

/// The per-pixel recipe derived from the camera colour data.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rendering {
    /// Camera values (0..1 of white level) to linear Rec. 2020, exposure included.
    pub matrix: [[f32; 3]; 3],
    /// Per-channel ceiling: where white-balanced highlights clip to neutral.
    pub clip: [f32; 3],
}

impl Rendering {
    /// Clips at the white-balanced saturation point (the neutral scaled so its
    /// largest channel is 1) and scales so that point has luminance 2^exposure.
    pub fn new(
        camera_to_rec2020: &Matrix,
        neutral: &Vector,
        exposure: f64,
    ) -> Result<Self, DecodeError> {
        if !is_positive(neutral) {
            return Err(DecodeError::corrupt("raw white balance is not positive"));
        }
        let largest = neutral.iter().copied().fold(0.0, f64::max);
        let clip = neutral.map(|v| v / largest);
        let white = apply(camera_to_rec2020, &clip);
        let luminance: f64 = white.iter().zip(REC2020_LUMINANCE).map(|(v, w)| v * w).sum();
        if !luminance.is_normal() || luminance < 0.0 {
            return Err(singular());
        }
        let scale = exposure.exp2() / luminance;
        Ok(Self {
            matrix: camera_to_rec2020.map(|row| row.map(|v| (v * scale) as f32)),
            clip: clip.map(|v| v as f32),
        })
    }

    /// Per-channel camera gains that white-balance the clip point to 1. Applied
    /// (and clipped) before demosaicing, they keep clipped highlights neutral.
    pub fn gains(&self) -> [f32; 3] {
        self.clip.map(|c| 1.0 / c)
    }

    /// This rendering for camera values already multiplied by `gains` and clipped.
    pub fn for_balanced(&self) -> Self {
        let matrix = self.matrix.map(|row| [0, 1, 2].map(|i| row[i] * self.clip[i]));
        Self { matrix, clip: [1.0; 3] }
    }

    /// Turns camera pixels (0..1 of white) into opaque linear Rec. 2020 in place.
    pub fn apply(&self, pixels: &mut [Rgba]) {
        let m = &self.matrix;
        parallel::for_each_chunk(pixels, |part| {
            for pixel in part {
                let [r, g, b] = [0, 1, 2].map(|i| pixel[i].min(self.clip[i]));
                for (row, value) in m.iter().zip(pixel.iter_mut()) {
                    *value = (row[0] * r + row[1] * g + row[2] * b).max(0.0);
                }
                pixel[3] = 1.0;
            }
        });
    }
}

pub fn is_positive(v: &Vector) -> bool {
    v.iter().all(|&x| x > 0.0 && x.is_finite())
}

impl CameraColor {
    /// A single-illuminant D65 colour matrix, as camera tables give it.
    pub fn d65(xyz_to_camera: Matrix, neutral: Vector) -> Self {
        let calibration = Calibration {
            temperature: D65_TEMPERATURE,
            color_matrix: xyz_to_camera,
            camera_calibration: IDENTITY,
            forward_matrix: None,
        };
        Self {
            calibration,
            second_calibration: None,
            analog_balance: [1.0; 3],
            as_shot: AsShot::Neutral(neutral),
            baseline_exposure: 0.0,
        }
    }

    fn interpolated(&self, white: (f64, f64)) -> Calibration {
        let first = &self.calibration;
        let Some(second) = &self.second_calibration else {
            return first.clone();
        };
        let inverse = 1.0 / temperature(white);
        let (low, high) = (1.0 / first.temperature, 1.0 / second.temperature);
        let weight =
            if low == high { 1.0 } else { ((inverse - high) / (low - high)).clamp(0.0, 1.0) };
        let forward_matrix = match (first.forward_matrix, second.forward_matrix) {
            (Some(a), Some(b)) => Some(lerp(&a, &b, weight)),
            _ => None,
        };
        Calibration {
            temperature: 1.0 / (weight * low + (1.0 - weight) * high),
            color_matrix: lerp(&first.color_matrix, &second.color_matrix, weight),
            camera_calibration: lerp(&first.camera_calibration, &second.camera_calibration, weight),
            forward_matrix,
        }
    }

    fn xyz_to_camera(&self, calibration: &Calibration) -> Matrix {
        let balance = multiply(&diagonal(&self.analog_balance), &calibration.camera_calibration);
        multiply(&balance, &calibration.color_matrix)
    }

    /// Finds the white chromaticity whose camera response is the given neutral.
    fn white_of_neutral(&self, neutral: &Vector) -> Result<(f64, f64), DecodeError> {
        let mut white = D50_XY;
        for _ in 0..WHITE_SEARCH_STEPS {
            let matrix = self.xyz_to_camera(&self.interpolated(white));
            let xyz = apply(&invert(&matrix).ok_or_else(singular)?, neutral);
            let next = xyz_to_xy(&xyz).ok_or_else(singular)?;
            let done = (next.0 - white.0).abs() + (next.1 - white.1).abs() < WHITE_SEARCH_TOLERANCE;
            white = next;
            if done {
                break;
            }
        }
        Ok(white)
    }

    pub fn rendering(&self) -> Result<Rendering, DecodeError> {
        let (white, neutral) = match self.as_shot {
            AsShot::Neutral(neutral) => (self.white_of_neutral(&neutral)?, neutral),
            AsShot::WhiteXy(white) => (white, self.neutral_of_white(white)),
            AsShot::Unknown => (D50_XY, self.neutral_of_white(D50_XY)),
        };
        if !is_positive(&neutral) {
            return Err(DecodeError::corrupt("DNG as-shot neutral is not positive"));
        }
        let calibration = self.interpolated(white);
        let balance = multiply(&diagonal(&self.analog_balance), &calibration.camera_calibration);
        let camera_to_d50 = match calibration.forward_matrix {
            Some(forward) => {
                let inverse_balance = invert(&balance).ok_or_else(singular)?;
                let reference = apply(&inverse_balance, &neutral);
                let white_balance = diagonal(&reference.map(|v| 1.0 / v));
                multiply(&forward, &multiply(&white_balance, &inverse_balance))
            }
            None => {
                let camera_to_xyz =
                    invert(&self.xyz_to_camera(&calibration)).ok_or_else(singular)?;
                multiply(&bradford(&xy_to_xyz(white), &xy_to_xyz(D50_XY)), &camera_to_xyz)
            }
        };
        let d50_to_d65 = bradford(&xy_to_xyz(D50_XY), &xy_to_xyz(D65_XY));
        let to_rec2020 = multiply(&XYZ_TO_REC2020, &multiply(&d50_to_d65, &camera_to_d50));
        Rendering::new(&to_rec2020, &neutral, self.baseline_exposure)
    }

    fn neutral_of_white(&self, white: (f64, f64)) -> Vector {
        apply(&self.xyz_to_camera(&self.interpolated(white)), &xy_to_xyz(white))
    }
}

fn singular() -> DecodeError {
    DecodeError::corrupt("camera colour matrices are singular")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn inverts_and_multiplies() {
        let m = [[2.0, 1.0, 0.0], [0.0, 1.0, 3.0], [1.0, 0.0, 1.0]];
        let product = multiply(&m, &invert(&m).unwrap());
        assert!(
            product.iter().flatten().zip(IDENTITY.iter().flatten()).all(|(a, b)| close(*a, *b))
        );
        assert!(invert(&[[1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [0.0, 0.0, 1.0]]).is_none());
    }

    #[test]
    fn bradford_maps_white_to_white() {
        let (d50, d65) = (xy_to_xyz(D50_XY), xy_to_xyz(D65_XY));
        let adapted = apply(&bradford(&d50, &d65), &d50);
        assert!(adapted.iter().zip(d65).all(|(a, b)| close(*a, b)));
    }

    #[test]
    fn temperatures_of_standard_illuminants() {
        assert!((temperature(D65_XY) - 6504.0).abs() < 30.0);
        assert!((temperature((0.4476, 0.4074)) - 2856.0).abs() < 30.0);
    }

    /// A camera that sees XYZ directly but with a weak red channel.
    fn camera(as_shot: AsShot) -> CameraColor {
        let color_matrix = [[0.5, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let calibration = Calibration {
            temperature: 6504.0,
            color_matrix,
            camera_calibration: IDENTITY,
            forward_matrix: None,
        };
        CameraColor {
            calibration,
            second_calibration: None,
            analog_balance: [1.0; 3],
            as_shot,
            baseline_exposure: 0.0,
        }
    }

    #[test]
    fn neutral_white_becomes_rec2020_white() {
        let d65 = xy_to_xyz(D65_XY);
        let neutral = [0.5 * d65[0], d65[1], d65[2]];
        let rendering = camera(AsShot::Neutral(neutral)).rendering().unwrap();
        let largest = neutral.iter().copied().fold(0.0, f64::max);
        let input = neutral.map(|v| (v / largest) as f32);
        let output =
            rendering.matrix.map(|row| row.iter().zip(input).map(|(m, v)| m * v).sum::<f32>());
        assert!(output.iter().all(|&v| (v - 1.0).abs() < 1e-3), "{output:?}");
        assert!(rendering.clip.iter().all(|&c| c > 0.0 && c <= 1.0));
    }

    #[test]
    fn white_xy_and_neutral_agree() {
        let from_xy = camera(AsShot::WhiteXy(D65_XY)).rendering().unwrap();
        let d65 = xy_to_xyz(D65_XY);
        let from_neutral =
            camera(AsShot::Neutral([0.5 * d65[0], d65[1], d65[2]])).rendering().unwrap();
        let pairs = from_xy.matrix.iter().flatten().zip(from_neutral.matrix.iter().flatten());
        assert!(pairs.into_iter().all(|(a, b)| (a - b).abs() < 1e-3));
    }

    #[test]
    fn interpolates_by_inverse_temperature() {
        let mut two = camera(AsShot::WhiteXy(D65_XY));
        let warm =
            Calibration { temperature: 2856.0, color_matrix: IDENTITY, ..two.calibration.clone() };
        two.second_calibration = Some(warm);
        let mid = two.interpolated(D65_XY);
        assert!(close(mid.color_matrix[0][0], 0.5));
        let between = two.interpolated((0.38, 0.38));
        assert!(between.color_matrix[0][0] > 0.5 && between.color_matrix[0][0] < 1.0);
    }
}
