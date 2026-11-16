//! Module: mirante_format_jpeg::dct
//! Purpose: Separable 8x8 forward and inverse DCTs (Arai-Agui-Nakajima, float).
//! Created: 2026-11-16
//! Architecture: The AAN butterflies leave every coefficient scaled by a known
//! factor, which is folded into the quantization tables built here, so the
//! transforms themselves need only five multiplications per row.

use std::f32::consts::{FRAC_1_SQRT_2, PI, SQRT_2};

/// Level shift between unsigned 8-bit samples and the signed DCT input.
const CENTER: f32 = 128.0;
const C2_MINUS_C6: f32 = 1.082_392_2;
const C2_PLUS_C6: f32 = 2.613_126;
const C2: f32 = 1.847_759;
/// Forward-transform rotator constants: c6, c2 - c6 and c2 + c6 (unscaled).
const FORWARD_C6: f32 = 0.382_683_43;
const FORWARD_C2_MINUS_C6: f32 = 0.541_196_1;
const FORWARD_C2_PLUS_C6: f32 = 1.306_563;

/// The AAN output scale of frequency `k`.
fn aan_scale(k: usize) -> f32 {
    if k == 0 { 1.0 } else { (k as f32 * PI / 16.0).cos() * SQRT_2 }
}

/// Dequantization multipliers in natural order, including the AAN scaling and
/// the 1/8 normalisation of the two passes.
pub fn inverse_table(quant: &[u16; 64]) -> [f32; 64] {
    std::array::from_fn(|i| f32::from(quant[i]) * aan_scale(i / 8) * aan_scale(i % 8) / 8.0)
}

/// Quantization reciprocals for the forward transform's scaled output.
pub fn forward_table(quant: &[u16; 64]) -> [f32; 64] {
    std::array::from_fn(|i| 1.0 / (f32::from(quant[i]) * aan_scale(i / 8) * aan_scale(i % 8) * 8.0))
}

/// Dequantizes one block and writes its 8x8 samples into `out` with `stride`.
pub fn inverse(coefficients: &[i16; 64], table: &[f32; 64], out: &mut [u8], stride: usize) {
    let mut work = [0.0_f32; 64];

    for column in 0..8 {
        let input: [f32; 8] = std::array::from_fn(|row| {
            f32::from(coefficients[row * 8 + column]) * table[row * 8 + column]
        });
        let output =
            if input[1..].iter().all(|&v| v == 0.0) { [input[0]; 8] } else { inverse_1d(input) };
        for (row, value) in output.into_iter().enumerate() {
            work[row * 8 + column] = value;
        }
    }
    for (row, line) in work.chunks_exact(8).enumerate() {
        let output = inverse_1d(line.try_into().expect("rows have eight values"));
        let target = &mut out[row * stride..row * stride + 8];
        for (sample, value) in target.iter_mut().zip(output) {
            // `as` saturates and maps NaN to 0, so damaged data cannot panic here.
            *sample = (value + CENTER + 0.5).clamp(0.0, 255.0) as u8;
        }
    }
}

fn inverse_1d(s: [f32; 8]) -> [f32; 8] {
    let (tmp10, tmp11) = (s[0] + s[4], s[0] - s[4]);
    let tmp13 = s[2] + s[6];
    let tmp12 = (s[2] - s[6]) * SQRT_2 - tmp13;
    let (even0, even3) = (tmp10 + tmp13, tmp10 - tmp13);
    let (even1, even2) = (tmp11 + tmp12, tmp11 - tmp12);

    let (z13, z10) = (s[5] + s[3], s[5] - s[3]);
    let (z11, z12) = (s[1] + s[7], s[1] - s[7]);
    let odd7 = z11 + z13;
    let tmp11 = (z11 - z13) * SQRT_2;
    let z5 = (z10 + z12) * C2;
    let tmp10 = C2_MINUS_C6 * z12 - z5;
    let tmp12 = -C2_PLUS_C6 * z10 + z5;
    let odd6 = tmp12 - odd7;
    let odd5 = tmp11 - odd6;
    let odd4 = tmp10 + odd5;

    [
        even0 + odd7,
        even1 + odd6,
        even2 + odd5,
        even3 - odd4,
        even3 + odd4,
        even2 - odd5,
        even1 - odd6,
        even0 - odd7,
    ]
}

/// Transforms level-shifted samples (natural order) into AAN-scaled coefficients;
/// multiply by `forward_table` to quantize.
pub fn forward(samples: &[f32; 64]) -> [f32; 64] {
    let mut work = [0.0_f32; 64];

    for (row, line) in samples.chunks_exact(8).enumerate() {
        let output = forward_1d(line.try_into().expect("rows have eight values"));
        work[row * 8..row * 8 + 8].copy_from_slice(&output);
    }
    let mut result = [0.0_f32; 64];
    for column in 0..8 {
        let output = forward_1d(std::array::from_fn(|row| work[row * 8 + column]));
        for (row, value) in output.into_iter().enumerate() {
            result[row * 8 + column] = value;
        }
    }
    result
}

fn forward_1d(d: [f32; 8]) -> [f32; 8] {
    let (tmp0, tmp7) = (d[0] + d[7], d[0] - d[7]);
    let (tmp1, tmp6) = (d[1] + d[6], d[1] - d[6]);
    let (tmp2, tmp5) = (d[2] + d[5], d[2] - d[5]);
    let (tmp3, tmp4) = (d[3] + d[4], d[3] - d[4]);

    let (tmp10, tmp13) = (tmp0 + tmp3, tmp0 - tmp3);
    let (tmp11, tmp12) = (tmp1 + tmp2, tmp1 - tmp2);
    let z1 = (tmp12 + tmp13) * FRAC_1_SQRT_2;

    let (tmp10o, tmp11o, tmp12o) = (tmp4 + tmp5, tmp5 + tmp6, tmp6 + tmp7);
    let z5 = (tmp10o - tmp12o) * FORWARD_C6;
    let z2 = FORWARD_C2_MINUS_C6 * tmp10o + z5;
    let z4 = FORWARD_C2_PLUS_C6 * tmp12o + z5;
    let z3 = tmp11o * FRAC_1_SQRT_2;
    let (z11, z13) = (tmp7 + z3, tmp7 - z3);

    [tmp10 + tmp11, z11 + z4, tmp13 + z1, z13 - z2, tmp10 - tmp11, z13 + z2, tmp13 - z1, z11 - z4]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn basis(frequency: usize, position: usize) -> f64 {
        let scale = if frequency == 0 { std::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
        let angle = (2 * position + 1) as f64 * frequency as f64 * std::f64::consts::PI / 16.0;
        scale * angle.cos()
    }

    /// A deterministic spread of coefficients with every frequency present.
    fn sample_coefficients() -> [i16; 64] {
        std::array::from_fn(|i| ((i * 37 + 11) % 41) as i16 - 20)
    }

    #[test]
    fn inverse_matches_the_definition() {
        let coefficients = sample_coefficients();
        let mut out = [0_u8; 64];
        inverse(&coefficients, &inverse_table(&[1; 64]), &mut out, 8);

        for (i, &sample) in out.iter().enumerate() {
            let (y, x) = (i / 8, i % 8);
            let mut sum = 0.0;
            for (j, &c) in coefficients.iter().enumerate() {
                sum += f64::from(c) * basis(j / 8, y) * basis(j % 8, x);
            }
            let expected = (sum / 4.0 + 128.0).round().clamp(0.0, 255.0);
            assert!((f64::from(sample) - expected).abs() <= 1.0, "sample {i}");
        }
    }

    #[test]
    fn forward_matches_the_definition() {
        let samples: [f32; 64] = std::array::from_fn(|i| ((i * 53 + 7) % 97) as f32 - 48.0);
        let table = forward_table(&[1; 64]);
        let result = forward(&samples);

        for (j, (&value, &scale)) in result.iter().zip(&table).enumerate() {
            let mut sum = 0.0;
            for (i, &s) in samples.iter().enumerate() {
                sum += f64::from(s) * basis(j / 8, i / 8) * basis(j % 8, i % 8);
            }
            assert!((f64::from(value * scale) - sum / 4.0).abs() < 1e-3, "coefficient {j}");
        }
    }
}
