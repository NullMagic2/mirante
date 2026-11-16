//! Module: mirante_format_heic::hevc::scaling
//! Purpose: scaling_list_data() and the ScalingFactor matrices (H.265 7.3.4, 7.4.5).
//! Created: 2026-11-16
//! Architecture: Parsed from the SPS or PPS by params.rs; transform.rs reads the
//! expanded per-size factors during dequantisation.

use super::bitstream::BitReader;
use super::scan::{DIAGONAL, SCANS};
use mirante_core::DecodeError;

/// Table 7-6, intra (matrixId 0..2) default for 8x8 and larger, in diagonal order.
const DEFAULT_INTRA: [u8; 64] = [
    16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 16, 17, 16, 17, 18, 17, 18, 18, 17, 18, 21, 19, 20,
    21, 20, 19, 21, 24, 22, 22, 24, 24, 22, 22, 24, 25, 25, 27, 30, 27, 25, 25, 29, 31, 35, 35, 31,
    29, 36, 41, 44, 41, 36, 47, 54, 54, 47, 65, 70, 65, 88, 88, 115,
];
/// Table 7-6, inter (matrixId 3..5) default.
const DEFAULT_INTER: [u8; 64] = [
    16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 17, 18, 18, 18, 18, 18, 18, 20, 20, 20,
    20, 20, 20, 20, 24, 24, 24, 24, 24, 24, 24, 24, 25, 25, 25, 25, 25, 25, 25, 28, 28, 28, 28, 28,
    28, 33, 33, 33, 33, 33, 41, 41, 41, 41, 54, 54, 54, 71, 71, 91,
];
const FLAT: u8 = 16;
const SIZE_IDS: usize = 4;
const MATRIX_IDS: usize = 6;

/// One coded list: up to 64 coefficients in diagonal order plus the DC value.
#[derive(Clone, Copy)]
struct List {
    coefficients: [u8; 64],
    dc: u8,
}

/// ScalingFactor[sizeId][matrixId], each stored row by row (index y * size + x).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScalingFactors {
    factors: [[Vec<u8>; MATRIX_IDS]; SIZE_IDS],
}

impl ScalingFactors {
    /// The default matrices used when scaling lists are on but none are coded.
    pub fn default_lists() -> Self {
        let lists =
            std::array::from_fn(|size_id| std::array::from_fn(|m| default_list(size_id, m)));
        Self::expand(&lists)
    }

    pub fn parse(reader: &mut BitReader<'_>) -> Result<Self, DecodeError> {
        let mut lists = [[default_list(0, 0); MATRIX_IDS]; SIZE_IDS];
        for (size_id, row) in lists.iter_mut().enumerate() {
            let step = if size_id == 3 { 3 } else { 1 };
            for matrix_id in (0..MATRIX_IDS).step_by(step) {
                row[matrix_id] = if reader.flag()? {
                    explicit_list(reader, size_id)?
                } else {
                    let delta = reader.ue_max((matrix_id / step) as u32, "scaling list delta")?;
                    match delta as usize * step {
                        0 => default_list(size_id, matrix_id),
                        offset => row[matrix_id - offset],
                    }
                };
            }
        }
        Ok(Self::expand(&lists))
    }

    /// The factor matrix for a block of `1 << log2_size` samples (2..=5).
    pub fn matrix(&self, log2_size: u32, matrix_id: usize) -> &[u8] {
        &self.factors[log2_size as usize - 2][matrix_id]
    }

    fn expand(lists: &[[List; MATRIX_IDS]; SIZE_IDS]) -> Self {
        let factors = std::array::from_fn(|size_id| {
            std::array::from_fn(|matrix_id| {
                // 32x32 chroma matrices (4:4:4 only) reuse the 16x16 lists.
                let source = if size_id == 3 && matrix_id % 3 != 0 { 2 } else { size_id };
                expand_list(&lists[source][matrix_id], size_id)
            })
        });
        Self { factors }
    }
}

fn default_list(size_id: usize, matrix_id: usize) -> List {
    let coefficients = match (size_id, matrix_id < 3) {
        (0, _) => [FLAT; 64],
        (_, true) => DEFAULT_INTRA,
        (_, false) => DEFAULT_INTER,
    };
    List { coefficients, dc: FLAT }
}

fn explicit_list(reader: &mut BitReader<'_>, size_id: usize) -> Result<List, DecodeError> {
    let count = if size_id == 0 { 16 } else { 64 };
    let mut next = 8_i32;
    let mut dc = FLAT;
    if size_id > 1 {
        next = reader.se_range(-7, 247, "scaling list DC")? + 8;
        dc = next as u8;
    }
    let mut coefficients = [0; 64];
    for coefficient in &mut coefficients[..count] {
        let delta = reader.se_range(-128, 127, "scaling list delta coefficient")?;
        next = (next + delta + 256) % 256;
        *coefficient = next as u8;
    }
    Ok(List { coefficients, dc })
}

/// Places a list on its block: 4x4 and 8x8 directly, larger sizes by replication.
fn expand_list(list: &List, size_id: usize) -> Vec<u8> {
    let size = 4 << size_id;
    let (grid_log2, scale) = if size_id == 0 { (2, 1) } else { (3, size / 8) };
    let scan = &SCANS[grid_log2][DIAGONAL];
    let mut out = vec![0; size * size];

    for (i, &(x, y)) in scan[..1 << (2 * grid_log2)].iter().enumerate() {
        for dy in 0..scale {
            for dx in 0..scale {
                let (px, py) = (usize::from(x) * scale + dx, usize::from(y) * scale + dy);
                out[py * size + px] = list.coefficients[i];
            }
        }
    }
    if size_id > 1 {
        out[0] = list.dc;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matrices_follow_table_7_6() {
        let factors = ScalingFactors::default_lists();
        assert!(factors.matrix(2, 0).iter().all(|&f| f == 16));
        let intra8 = factors.matrix(3, 0);
        assert_eq!((intra8[0], intra8[63], intra8[7]), (16, 115, 24));
        let intra32 = factors.matrix(5, 1);
        assert_eq!((intra32[0], intra32[32 * 32 - 1]), (16, 115));
    }
}
