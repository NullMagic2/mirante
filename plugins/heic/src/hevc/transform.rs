//! Module: mirante_format_heic::hevc::transform
//! Purpose: Scaling and transformation (H.265 8.6.2-8.6.4): dequantisation with
//! scaling lists, inverse DST/DCT, transform skip, bypass, rotation and RDPCM.
//! Created: 2026-11-16
//! Architecture: residual.rs supplies TransCoeffLevel; ctu.rs adds the residual
//! this module returns to the intra prediction.

/// levelScale[qP % 6].
const LEVEL_SCALE: [i64; 6] = [40, 45, 51, 57, 64, 72];
const COEFF_MIN: i32 = -(1 << 15);
const COEFF_MAX: i32 = (1 << 15) - 1;
const FLAT_SCALE: i64 = 16;
/// Maximum block side and coefficient count.
pub const MAX_SIZE: usize = 32;
pub const MAX_COEFFS: usize = MAX_SIZE * MAX_SIZE;

/// The 4x4 DST-VII basis (8.6.4.2, trType 1), row k is basis function k.
const DST: [[i32; 4]; 4] =
    [[29, 55, 74, 84], [74, 74, 0, -74], [84, -29, -74, 55], [55, -84, 74, -29]];

/// The 32-point DCT basis, built from the odd-angle coefficient set of 8.6.4.2.
static DCT: [[i8; 32]; 32] = dct_matrix();

/// 64 * sqrt(2) * cos(m * pi / 64) as the standard rounds it, for m = 0..=32.
const COSINES: [i8; 33] = [
    64, 90, 90, 90, 89, 88, 87, 85, 83, 82, 80, 78, 75, 73, 70, 67, 64, 61, 57, 54, 50, 46, 43, 38,
    36, 31, 25, 22, 18, 13, 9, 4, 0,
];

const fn dct_matrix() -> [[i8; 32]; 32] {
    let mut matrix = [[0; 32]; 32];
    let mut k = 0;
    while k < 32 {
        let mut n = 0;
        while n < 32 {
            let mut m = ((2 * n + 1) * k) % 128;
            if m > 64 {
                m = 128 - m;
            }
            matrix[k][n] = if k == 0 {
                64
            } else if m > 32 {
                -COSINES[64 - m]
            } else {
                COSINES[m]
            };
            n += 1;
        }
        k += 1;
    }
    matrix
}

/// Everything about a transform block that decides how its residual is formed.
#[derive(Clone, Copy)]
pub struct BlockParams {
    pub log2_size: u32,
    pub bit_depth: u32,
    /// Qp' including QpBdOffset.
    pub qp: i32,
    pub transform_skip: bool,
    pub bypass: bool,
    /// Luma 4x4 intra blocks use the DST.
    pub dst: bool,
    /// transform_skip_rotation_enabled_flag for a 4x4 block.
    pub rotate: bool,
    /// Implicit RDPCM direction: Some(true) horizontal, Some(false) vertical.
    pub rdpcm: Option<bool>,
    /// Bottom-right extent of the nonzero coefficients, to skip zero work.
    pub extent: (usize, usize),
}

/// Turns coefficients (row by row, `1 << log2_size` wide) into residuals in place.
/// `scaling` is the ScalingFactor matrix, or None for flat scaling.
pub fn residual(coeffs: &mut [i32], params: &BlockParams, scaling: Option<&[u8]>) {
    let size = 1 << params.log2_size;
    let block = &mut coeffs[..size * size];
    if params.bypass {
        if params.rotate {
            block.reverse();
        }
    } else {
        dequantise(block, params, scaling);
        let shift = 20 - params.bit_depth;
        if params.transform_skip {
            let ts_shift = 5 + params.log2_size;
            if params.rotate {
                block.reverse();
            }
            for value in block.iter_mut() {
                *value = ((*value << ts_shift) + (1 << (shift - 1))) >> shift;
            }
        } else {
            inverse_transform(block, params);
            for value in block.iter_mut() {
                *value = (*value + (1 << (shift - 1))) >> shift;
            }
        }
    }
    match params.rdpcm {
        Some(true) => {
            for row in block.chunks_exact_mut(size) {
                for x in 1..size {
                    row[x] += row[x - 1];
                }
            }
        }
        Some(false) => {
            for i in size..size * size {
                block[i] += block[i - size];
            }
        }
        None => {}
    }
}

fn dequantise(block: &mut [i32], params: &BlockParams, scaling: Option<&[u8]>) {
    let shift = params.bit_depth + params.log2_size - 5;
    let round = 1_i64 << (shift - 1);
    let level = LEVEL_SCALE[(params.qp % 6) as usize] << (params.qp / 6);
    let size = 1 << params.log2_size;
    let (width, height) = (params.extent.0 + 1, params.extent.1 + 1);
    // Transform-skipped blocks larger than 4x4 ignore the scaling list.
    let scaling = scaling.filter(|_| !(params.transform_skip && size > 4));

    for y in 0..height {
        for x in 0..width {
            let value = &mut block[y * size + x];
            if *value == 0 {
                continue;
            }
            let m = scaling.map_or(FLAT_SCALE, |s| i64::from(s[y * size + x]));
            let scaled = (i64::from(*value) * m * level + round) >> shift;
            *value = scaled.clamp(i64::from(COEFF_MIN), i64::from(COEFF_MAX)) as i32;
        }
    }
}

/// 8.6.4.2: columns first, clipping to 16 bits between the two stages.
fn inverse_transform(block: &mut [i32], params: &BlockParams) {
    let size = 1 << params.log2_size;
    let (columns, rows) = (params.extent.0 + 1, params.extent.1 + 1);
    let step = 32 >> params.log2_size;
    let basis = |k: usize, n: usize| -> i32 {
        if params.dst { DST[k][n] } else { i32::from(DCT[k * step][n]) }
    };
    let mut column_out = [0_i32; MAX_COEFFS];
    for x in 0..columns {
        for n in 0..size {
            let sum: i32 = (0..rows).map(|k| basis(k, n) * block[k * size + x]).sum();
            column_out[n * size + x] = ((sum + 64) >> 7).clamp(COEFF_MIN, COEFF_MAX);
        }
    }
    for y in 0..size {
        let row = &column_out[y * size..y * size + size];
        for n in 0..size {
            block[y * size + n] = (0..columns).map(|k| basis(k, n) * row[k]).sum();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dct_matrix_matches_the_standard() {
        assert_eq!(DCT[0][31], 64);
        assert_eq!(DCT[1][..4], [90, 90, 88, 85]);
        assert_eq!(DCT[8][..4], [83, 36, -36, -83]);
        assert_eq!(DCT[16][..2], [64, -64]);
        assert_eq!(DCT[31][..2], [4, -13]);
    }

    #[test]
    fn dc_coefficient_gives_a_flat_residual() {
        let mut coeffs = [0; MAX_COEFFS];
        coeffs[0] = 64;
        let params = BlockParams {
            log2_size: 3,
            bit_depth: 8,
            qp: 4,
            transform_skip: false,
            bypass: false,
            dst: false,
            rotate: false,
            rdpcm: None,
            extent: (0, 0),
        };
        residual(&mut coeffs, &params, None);
        // d = (64 * 16 * 64 + 8) >> 6 = 1024; each stage scales by 64 / 128 then 64 / 4096.
        assert!(coeffs[..64].iter().all(|&r| r == coeffs[0]));
        assert_eq!(coeffs[0], 8);
    }
}
