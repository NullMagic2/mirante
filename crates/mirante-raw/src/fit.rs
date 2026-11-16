//! Module: mirante_raw::fit
//! Purpose: Estimates camera colour from the camera's own JPEG preview.
//! Created: 2026-11-16
//! Architecture: For cameras the table lacks, or files without white balance.
//! Both images become small thumbnails (the camera's binned straight from the
//! mosaic; clipped areas masked through alpha) paired by position. A camera to
//! linear Rec. 2020 matrix is fitted by reweighted least squares on colour
//! alone (each sample's brightness is matched, so the preview's exposure and
//! tone curve drop out), regularised toward a prior, with white kept neutral.

use crate::color::{self, Matrix, REC2020_LUMINANCE, Vector};
use crate::demosaic::CfaPattern;
use mirante_core::{ColorSpace, Image, parallel};
use mirante_edit::downscale_to_fit;

/// Thumbnail side: enough samples for a 3x3 fit, few enough to be instant.
const THUMBNAIL_SIZE: u32 = 160;
/// Raw values (0..1 of white) and encoded preview values counted as clipped.
const CLIPPED_RAW: f32 = 0.95;
const CLIPPED_PREVIEW: f32 = 0.97;
/// A thumbnail pixel is usable only if none of its source pixels was clipped.
const UNCLIPPED: f32 = 0.999;
/// Darker preview samples are mostly noise and the tone curve's toe.
const MIN_TARGET_LUMINANCE: f64 = 0.004;
const MIN_CAMERA_VALUE: f64 = 1e-4;
const MIN_SAMPLES: usize = 64;
/// Reweighting rounds; the fit itself is linear.
const ITERATIONS: usize = 6;
/// Ridge strength toward the prior, relative to the data's mean curvature.
const REGULARISATION: f64 = 0.005;
/// Cauchy weighting scale in robust standard deviations.
const ROBUST_SCALE: f64 = 2.4;
const MIN_RESIDUAL: f64 = 1e-3;

/// A thumbnail pixel of the camera image and the preview's colour at the same spot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// Camera RGB, 0..1 of white, not white balanced.
    pub camera: Vector,
    /// The preview's linear Rec. 2020 colour, tone curve included.
    pub target: Vector,
}

/// An upright camera thumbnail straight from normalised photosites (one sample
/// per site with a CFA pattern, else three): each pixel averages each colour
/// over a block of sites, and is transparent if any of them is clipped.
pub fn camera_thumbnail(
    values: &[f32],
    (width, height): (usize, usize),
    pattern: Option<&CfaPattern>,
    orientation: u16,
) -> Option<Image> {
    let pattern_side = pattern.map_or(1, |p| p.width().max(p.height()));
    let block = width.max(height).div_ceil(THUMBNAIL_SIZE as usize).max(pattern_side);
    let (columns, rows) = (width / block, height / block);
    let mut sums = vec![[0.0_f32; 4]; columns * rows];
    let mut counts = vec![[0_u32; 3]; columns * rows];
    for y in 0..rows * block {
        let row = (y / block) * columns;
        for x in 0..columns * block {
            let (sum, count) = (&mut sums[row + x / block], &mut counts[row + x / block]);
            let mut add = |color: usize, value: f32| {
                sum[color] += value;
                count[color] += 1;
                // Alpha counts clipped sites for now.
                if value >= CLIPPED_RAW {
                    sum[3] += 1.0;
                }
            };
            match pattern {
                Some(pattern) => add(usize::from(pattern.color_at(x, y)), values[y * width + x]),
                None => (0..3).for_each(|c| add(c, values[(y * width + x) * 3 + c])),
            }
        }
    }
    let pixels = sums
        .iter()
        .zip(&counts)
        .map(|(sum, count)| {
            let mean = [0, 1, 2].map(|c| sum[c] / count[c].max(1) as f32);
            [mean[0], mean[1], mean[2], if sum[3] > 0.0 { 0.0 } else { 1.0 }]
        })
        .collect();
    let image = Image::new(columns as u32, rows as u32, pixels, ColorSpace::LinearRec2020).ok()?;
    Some(image.oriented(orientation))
}

/// The decoded preview, already upright, as a linear Rec. 2020 thumbnail with
/// clipped areas transparent.
pub fn preview_thumbnail(mut preview: Image) -> Image {
    parallel::for_each_chunk(preview.pixels_mut(), |part| {
        for pixel in part {
            pixel[3] = if pixel[..3].iter().any(|&v| v >= CLIPPED_PREVIEW) { 0.0 } else { 1.0 };
        }
    });
    downscale_to_fit(&mirante_color::to_working(preview), THUMBNAIL_SIZE, THUMBNAIL_SIZE)
}

/// Pairs each preview thumbnail pixel with the camera thumbnail pixel at the
/// same place, assuming the preview shows the centre of the sensor image.
pub fn samples(camera: &Image, preview: &Image) -> Vec<Sample> {
    let (cw, ch) = (f64::from(camera.width()), f64::from(camera.height()));
    let (pw, ph) = (f64::from(preview.width()), f64::from(preview.height()));
    let aspect = pw / ph;
    let (used_w, used_h) = if cw / ch > aspect { (ch * aspect, ch) } else { (cw, cw / aspect) };
    let (left, top) = ((cw - used_w) / 2.0, (ch - used_h) / 2.0);
    let mut out = Vec::new();
    for py in 0..preview.height() {
        for (px, target) in preview.row(py).iter().enumerate() {
            let cx = (left + (px as f64 + 0.5) / pw * used_w) as u32;
            let cy = (top + (f64::from(py) + 0.5) / ph * used_h) as u32;
            let source =
                camera.row(cy.min(camera.height() - 1))[(cx.min(camera.width() - 1)) as usize];
            let sample = Sample {
                camera: [0, 1, 2].map(|i| f64::from(source[i])),
                target: [0, 1, 2].map(|i| f64::from(target[i])),
            };
            let usable = source[3] >= UNCLIPPED
                && target[3] >= UNCLIPPED
                && luminance(&sample.target) >= MIN_TARGET_LUMINANCE
                && sample.camera.iter().all(|&v| v >= MIN_CAMERA_VALUE);
            if usable {
                out.push(sample);
            }
        }
    }
    out
}

fn luminance(v: &Vector) -> f64 {
    v.iter().zip(REC2020_LUMINANCE).map(|(a, b)| a * b).sum()
}

fn dot(a: &Vector, b: &Vector) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Scales a neutral so its largest channel is 1; None unless all positive.
fn normalised(neutral: Vector) -> Option<Vector> {
    let largest = neutral.iter().copied().fold(0.0, f64::max);
    let scaled = neutral.map(|v| v / largest);
    color::is_positive(&scaled).then_some(scaled)
}

/// Grey world: the mean colour of the unclipped pixels is taken to be neutral.
pub fn grey_world(camera: &Image) -> Option<Vector> {
    let mut sum = [0.0; 3];
    for pixel in camera.pixels().iter().filter(|p| p[3] >= UNCLIPPED) {
        (0..3).for_each(|i| sum[i] += f64::from(pixel[i]));
    }
    normalised(sum)
}

/// Gaussian elimination with partial pivoting; None if singular.
fn solve<const N: usize>(mut a: [[f64; N]; N], mut b: [f64; N]) -> Option<[f64; N]> {
    for column in 0..N {
        let pivot =
            (column..N).max_by(|&i, &j| a[i][column].abs().total_cmp(&a[j][column].abs()))?;
        if !a[pivot][column].is_normal() {
            return None;
        }
        a.swap(column, pivot);
        b.swap(column, pivot);
        let (done, rest) = a.split_at_mut(column + 1);
        let pivot_row = &done[column];
        for (offset, row) in rest.iter_mut().enumerate() {
            let factor = row[column] / pivot_row[column];
            for (value, pivot_value) in row[column..].iter_mut().zip(&pivot_row[column..]) {
                *value -= factor * pivot_value;
            }
            b[column + 1 + offset] -= factor * b[column];
        }
    }
    let mut x = [0.0; N];
    for row in (0..N).rev() {
        let rest: f64 = (row + 1..N).map(|k| a[row][k] * x[k]).sum();
        x[row] = (b[row] - rest) / a[row][row];
    }
    x.iter().all(|v| v.is_finite()).then_some(x)
}

/// The part of `v` not along `t`: what a colour error looks like once the
/// brightness of the two is matched.
fn reject(v: &Vector, t: &Vector) -> Vector {
    let k = dot(v, t) / dot(t, t);
    [0, 1, 2].map(|i| v[i] - k * t[i])
}

const UNIT: [Vector; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// Least squares on colour for the white-balanced matrix `prior` + D: D's rows
/// sum to zero (white stays white) and are penalised. Errors are measured after
/// matching each sample's brightness, so exposure and tone curve drop out.
fn balanced_fit(
    balanced_cameras: &[Vector],
    targets: &[Vector],
    weights: &[f64],
    prior: &Matrix,
) -> Option<Matrix> {
    let mut normal = [[0.0; 6]; 6];
    let mut right = [0.0; 6];
    for ((b, t), &w) in balanced_cameras.iter().zip(targets).zip(weights) {
        let w = w / dot(b, b);
        let base = reject(&color::apply(prior, b), t);
        let rejected = UNIT.map(|e| reject(&e, t));
        let features = [b[0] - b[2], b[1] - b[2]];
        // Parameter 2i + m adds features[m] to row i (and subtracts it from column 2).
        let columns: [Vector; 6] =
            std::array::from_fn(|j| rejected[j / 2].map(|v| v * features[j % 2]));
        for ((normal_row, right_value), column) in normal.iter_mut().zip(&mut right).zip(&columns) {
            *right_value -= w * dot(column, &base);
            for (value, other) in normal_row.iter_mut().zip(&columns) {
                *value += w * dot(column, other);
            }
        }
    }
    // |D|^2 per row is d0^2 + d1^2 + (d0 + d1)^2.
    let lambda = REGULARISATION * (0..6).map(|i| normal[i][i]).sum::<f64>() / 6.0;
    for row in 0..3 {
        let (a, b) = (2 * row, 2 * row + 1);
        normal[a][a] += 2.0 * lambda;
        normal[b][b] += 2.0 * lambda;
        normal[a][b] += lambda;
        normal[b][a] += lambda;
    }
    let x = solve(normal, right)?;
    let row = |i: usize| {
        let (d0, d1) = (x[2 * i], x[2 * i + 1]);
        [prior[i][0] + d0, prior[i][1] + d1, prior[i][2] - d0 - d1]
    };
    Some([row(0), row(1), row(2)])
}

/// White balance from a free colour fit: the camera colour the fitted matrix maps
/// to grey. The ridge pulls toward the prior balanced for `guess`; the mean camera
/// colour's luminance is pinned at 1 to fix the scale.
fn free_neutral(
    cameras: &[Vector],
    targets: &[Vector],
    weights: &[f64],
    prior: &Matrix,
    guess: &Vector,
) -> Option<Vector> {
    let count = cameras.len() as f64;
    let mean = cameras.iter().fold([0.0; 3], |a, c| [0, 1, 2].map(|i| a[i] + c[i] / count));
    let start = color::multiply(prior, &color::diagonal(&guess.map(|v| 1.0 / v)));
    let start_luminance = luminance(&color::apply(&start, &mean));
    if !start_luminance.is_normal() {
        return None;
    }
    let start = start.map(|row| row.map(|v| v / start_luminance));
    // Unknowns 0..9 are the matrix row by row; 9 is the scale constraint's multiplier.
    let mut normal = [[0.0; 10]; 10];
    let mut right = [0.0; 10];
    for ((c, t), &w) in cameras.iter().zip(targets).zip(weights) {
        let w = w / dot(c, c);
        let rejected = UNIT.map(|e| reject(&e, t));
        for r in 0..9 {
            for k in 0..9 {
                let overlap = dot(&rejected[r / 3], &rejected[k / 3]);
                normal[r][k] += w * c[r % 3] * c[k % 3] * overlap;
            }
        }
    }
    let lambda = REGULARISATION * (0..9).map(|i| normal[i][i]).sum::<f64>() / 9.0;
    for r in 0..9 {
        normal[r][r] += lambda;
        right[r] = lambda * start[r / 3][r % 3];
        let gradient = REC2020_LUMINANCE[r / 3] * mean[r % 3];
        normal[r][9] = gradient;
        normal[9][r] = gradient;
    }
    right[9] = 1.0;
    let x = solve(normal, right)?;
    let fitted = [0, 1, 2].map(|r| [0, 1, 2].map(|k| x[3 * r + k]));
    normalised(color::apply(&color::invert(&fitted)?, &[1.0; 3]))
}

/// Fits camera to linear Rec. 2020 against preview samples, given a balanced
/// `prior` (rows sum to 1) and the camera colour of white if known. Returns the
/// matrix for unbalanced input and the neutral, or None if samples are too few.
pub fn fit_matrix(
    samples: &[Sample],
    prior: &Matrix,
    neutral: Option<Vector>,
) -> Option<(Matrix, Vector)> {
    if samples.len() < MIN_SAMPLES {
        return None;
    }
    let cameras: Vec<Vector> = samples.iter().map(|s| s.camera).collect();
    let targets: Vec<Vector> = samples.iter().map(|s| s.target).collect();
    let mut white = match neutral {
        Some(known) => normalised(known)?,
        None => normalised(cameras.iter().fold([0.0; 3], |a, c| [0, 1, 2].map(|i| a[i] + c[i])))?,
    };
    let mut weights = vec![1.0; samples.len()];
    let mut balanced = *prior;
    for _ in 0..ITERATIONS {
        if neutral.is_none() {
            white = free_neutral(&cameras, &targets, &weights, prior, &white).unwrap_or(white);
        }
        let balanced_cameras: Vec<Vector> =
            cameras.iter().map(|c| [0, 1, 2].map(|i| c[i] / white[i])).collect();
        balanced = balanced_fit(&balanced_cameras, &targets, &weights, prior)?;
        weights = robust_weights(&balanced, &balanced_cameras, &targets);
    }
    let matrix = color::multiply(&balanced, &color::diagonal(&white.map(|v| 1.0 / v)));
    Some((matrix, white))
}

/// Cauchy weights on the relative colour error, so outliers (misaligned edges,
/// local adjustments in the preview) barely count.
fn robust_weights(balanced: &Matrix, cameras: &[Vector], targets: &[Vector]) -> Vec<f64> {
    let residuals: Vec<f64> = cameras
        .iter()
        .zip(targets)
        .map(|(b, t)| {
            let predicted = color::apply(balanced, b);
            let error = reject(&predicted, t);
            dot(&error, &error).sqrt() / dot(&predicted, &predicted).sqrt().max(MIN_CAMERA_VALUE)
        })
        .collect();
    let mut sorted = residuals.clone();
    sorted.sort_by(f64::total_cmp);
    let sigma = (1.4826 * sorted[sorted.len() / 2]).max(MIN_RESIDUAL);
    residuals.iter().map(|r| 1.0 / (1.0 + (r / (ROBUST_SCALE * sigma)).powi(2))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A camera with crosstalk: its own matrix to Rec. 2020 and a warm neutral.
    const TRUE_MATRIX: Matrix = [[1.9, -0.7, -0.2], [-0.25, 1.6, -0.35], [0.05, -0.55, 1.5]];

    /// A plausible but different camera, as the maker average would be.
    const PRIOR: Matrix = [[1.5, -0.4, -0.1], [-0.2, 1.4, -0.2], [0.0, -0.4, 1.4]];

    fn random(seed: &mut u64) -> f64 {
        *seed =
            seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (*seed >> 11) as f64 / (1_u64 << 53) as f64
    }

    /// Scene colours seen by the camera, and a preview made with a tone curve.
    fn synthetic(outliers: bool) -> Vec<Sample> {
        let mut seed = 7;
        let neutral = [0.5, 1.0, 0.7];
        let to_rec2020 = color::multiply(&TRUE_MATRIX, &color::diagonal(&neutral.map(|v| 1.0 / v)));
        let to_camera = color::invert(&to_rec2020).unwrap();
        (0..2000)
            .filter_map(|k| {
                let brightness = 0.02 + 0.5 * random(&mut seed);
                let tint = [0, 1, 2].map(|_| 0.4 + random(&mut seed));
                let scene = tint.map(|v| v * brightness);
                let camera = color::apply(&to_camera, &scene);
                let gain = 1.6 * luminance(&scene).powf(-0.2);
                let mut target = scene.map(|v| v * gain);
                if outliers && k % 10 == 0 {
                    target = [target[2], target[0], target[1]];
                }
                camera.iter().all(|&v| v > 0.0).then_some(Sample { camera, target })
            })
            .collect()
    }

    /// Rows of a matrix after scaling white to luminance 1.
    fn shape(m: &Matrix, neutral: &Vector) -> Matrix {
        let y = luminance(&color::apply(m, neutral));
        m.map(|row| row.map(|v| v / y))
    }

    fn assert_recovers(found: &Matrix, white: &Vector) {
        let expected = shape(
            &color::multiply(&TRUE_MATRIX, &color::diagonal(&[2.0, 1.0, 1.0 / 0.7])),
            &[0.5, 1.0, 0.7],
        );
        let found = shape(found, white);
        // The ridge pulls weakly constrained directions slightly toward the prior.
        for (a, b) in found.iter().flatten().zip(expected.iter().flatten()) {
            assert!((a - b).abs() < 0.12, "{found:?}\n{expected:?}");
        }
    }

    #[test]
    fn recovers_a_known_matrix_through_a_tone_curve() {
        let (matrix, white) = fit_matrix(&synthetic(true), &PRIOR, Some([0.5, 1.0, 0.7])).unwrap();
        assert_recovers(&matrix, &white);
    }

    #[test]
    fn finds_the_white_balance_when_unknown() {
        let (matrix, white) = fit_matrix(&synthetic(false), &PRIOR, None).unwrap();
        assert!(white.iter().zip([0.5, 1.0, 0.7]).all(|(a, b)| (a - b).abs() < 0.03), "{white:?}");
        assert_recovers(&matrix, &white);
    }

    #[test]
    fn too_few_samples_give_no_fit() {
        assert!(fit_matrix(&synthetic(false)[..10], &color::IDENTITY, None).is_none());
    }

    #[test]
    fn solves_linear_systems() {
        let x = solve([[2.0, 1.0], [1.0, 3.0]], [3.0, 5.0]).unwrap();
        assert!((x[0] - 0.8).abs() < 1e-12 && (x[1] - 1.4).abs() < 1e-12);
        assert!(solve([[1.0, 2.0], [2.0, 4.0]], [1.0, 2.0]).is_none());
    }
}
