//! Module: mirante_raw_canon::crx::quant
//! Purpose: Inverse quantisation of lossy CRX subbands: version 1 uses one
//! parameter per subband, version 2 a per-tile map of quantisation parameters
//! (QP) coded at the start of the tile data.
//! Created: 2026-11-16
//! Architecture: The QP map uses the same median predictor and Golomb codes as
//! band.rs. Ported from LibRaw's crx.cpp (crxMakeQStep, crxDecodeGolomb*).

use super::bits::{BitReader, median, next_k, signed};
use super::header::Band;
use mirante_core::DecodeError;

/// Quantiser step for parameter q is STEP_BASE[q % 6] * 2^(q / 6 - 6).
const STEP_BASE: [u32; 6] = [0x28, 0x2D, 0x33, 0x39, 0x40, 0x48];
const MAX_STEP: u64 = 0x16_8000;
const QP_K_LIMIT: u32 = 7;
const QP_ESCAPE_ZEROS: u32 = 23;
const QP_ESCAPE_BITS: u32 = 8;
const QP_OFFSET: i32 = 4;
/// QP map cells cover 8 plane columns by 2 plane rows.
const QP_CELL_WIDTH: usize = 8;
const MAX_QP: i32 = 6 * 30;

pub fn step(q: i32) -> Result<u32, DecodeError> {
    if !(0..=MAX_QP).contains(&q) {
        return Err(DecodeError::corrupt("CRX quantisation parameter out of range"));
    }
    let (base, octave) = (STEP_BASE[(q % 6) as usize], (q / 6) as u32);
    Ok(if octave >= 6 { base << (octave - 6) } else { base >> (6 - octave) })
}

/// Quantiser steps for one decomposition level, one per QP map cell.
#[derive(Debug)]
pub struct StepMap {
    pub width: usize,
    pub height: usize,
    pub steps: Vec<u32>,
}

/// Step maps for each level, from the deepest (coarsest) to the finest.
pub fn step_maps(
    data: &[u8],
    tile_width: usize,
    tile_height: usize,
    levels: u8,
) -> Result<Vec<StepMap>, DecodeError> {
    let width = tile_width.div_ceil(QP_CELL_WIDTH);
    let height = tile_height.div_ceil(2);
    let qp = read_qp(data, width, height)?;
    let averaged = |rows: usize| -> Result<StepMap, DecodeError> {
        let map_height = tile_height.div_ceil(2 * rows);
        let mut steps = Vec::with_capacity(width * map_height);
        for row in 0..map_height {
            for column in 0..width {
                let sum: i32 =
                    (0..rows).map(|r| qp[(rows * row + r).min(height - 1) * width + column]).sum();
                // C division truncates towards zero.
                steps.push(step(sum / rows as i32)?);
            }
        }
        Ok(StepMap { width, height: map_height, steps })
    };
    let mut maps = Vec::new();
    for level in 0..levels {
        maps.push(averaged(1 << (levels - 1 - level))?);
    }
    Ok(maps)
}

/// The QP map, one row of cells per two plane rows.
fn read_qp(data: &[u8], width: usize, height: usize) -> Result<Vec<i32>, DecodeError> {
    let mut bits = BitReader::new(data);
    let mut k = 0;
    let mut previous = vec![0_i32; width + 2];
    let mut current = vec![0_i32; width + 2];
    let mut qp = Vec::with_capacity(width * height);
    for row in 0..height {
        std::mem::swap(&mut previous, &mut current);
        if row == 0 {
            current[0] = 0;
            for column in 0..width {
                let code = read_code(&mut bits, k)?;
                current[column + 1] = current[column].wrapping_add(signed(code));
                k = next_k(k, code, QP_K_LIMIT);
            }
        } else {
            current[0] = previous[1];
            for column in 0..width {
                let prediction = median(current[column], previous[column + 1], previous[column]);
                let code = read_code(&mut bits, k)?;
                current[column + 1] = prediction.wrapping_add(signed(code));
                let context = if column + 1 < width {
                    let step = previous[column + 2].wrapping_sub(previous[column + 1]);
                    code.wrapping_add(step.wrapping_mul(2).unsigned_abs()) >> 1
                } else {
                    code
                };
                k = next_k(k, context, QP_K_LIMIT);
            }
        }
        current[width + 1] = current[width].wrapping_add(1);
        qp.extend(current[1..=width].iter().map(|&v| v.wrapping_add(QP_OFFSET)));
    }
    Ok(qp)
}

fn read_code(bits: &mut BitReader<'_>, k: u32) -> Result<u32, DecodeError> {
    let zeros = bits.zeros()?;
    Ok(if zeros >= QP_ESCAPE_ZEROS {
        bits.bits(QP_ESCAPE_BITS)
    } else if k > 0 {
        (zeros << k) | bits.bits(k)
    } else {
        zeros
    })
}

/// Scales one decoded subband row back to coefficient units.
pub fn dequantize_row(
    values: &mut [i32],
    band: &Band,
    map: Option<&StepMap>,
    row: usize,
    shift: u32,
) -> Result<(), DecodeError> {
    match map {
        Some(map) => {
            let cells = &map.steps[row.min(map.height - 1) * map.width..][..map.width];
            for (column, value) in values.iter_mut().enumerate() {
                let cell = cells[(column >> shift).min(map.width - 1)];
                let scaled = (u64::from(cell) * u64::from(band.q_step_mult)) >> 3;
                let step = (u64::from(band.q_step_base) + scaled).clamp(1, MAX_STEP);
                *value = value.wrapping_mul(step as i32);
            }
        }
        None => {
            let step = step(band.q_param as i32)?;
            if step != 1 {
                values.iter_mut().for_each(|value| *value = value.wrapping_mul(step as i32));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_follow_the_table() {
        assert_eq!(step(0).unwrap(), 0x28 >> 6);
        assert_eq!(step(36).unwrap(), 0x28);
        assert_eq!(step(43).unwrap(), 0x2D << 1);
        assert!(step(-1).is_err());
    }

    #[test]
    fn averages_qp_rows_per_level() {
        // A 16x4 tile has a 2x2 QP map. All-zero codes keep every QP at 0 + 4.
        let maps = step_maps(&[0xFF; 4], 16, 4, 2).unwrap();
        assert_eq!((maps[0].width, maps[0].height), (2, 1));
        assert_eq!((maps[1].width, maps[1].height), (2, 2));
        assert!(maps.iter().all(|map| map.steps.iter().all(|&s| s == step(4).unwrap())));
    }
}
