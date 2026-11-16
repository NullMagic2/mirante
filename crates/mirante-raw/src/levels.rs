//! Module: mirante_raw::levels
//! Purpose: Scales raw samples so black is 0 and saturation is 1.
//! Created: 2026-11-16
//! Architecture: One implementation for DNG (linearisation table, black pattern
//! and per-row/column deltas, white per sample) and for maker decoders (a black
//! block and one white level). Rows are processed in parallel.

use crate::frame::{Area, BlackLevel};
use crate::unpack::RawPlane;
use mirante_core::{DecodeError, parallel};

/// Black and white levels of a raw plane.
#[derive(Clone, Debug, PartialEq)]
pub struct Levels {
    /// Repeating black block over sample columns (pixel column x samples per
    /// pixel + sample) and rows.
    pub black: BlackLevel,
    /// Plane sample column and row where the block's first value lies.
    pub black_origin: (usize, usize),
    /// Added black per column and per row of the area (DNG BlackLevelDeltaH/V);
    /// empty when absent.
    pub black_delta_columns: Vec<f32>,
    pub black_delta_rows: Vec<f32>,
    /// Saturation per sample, in linearised units.
    pub white: Vec<f32>,
    pub linearization: Option<Vec<u16>>,
}

impl Levels {
    /// The levels of a maker decoder: one black block aligned to the plane, one
    /// white for all `samples_per_pixel` samples.
    pub fn simple(black: BlackLevel, white: f32, samples_per_pixel: usize) -> Self {
        Self {
            black,
            black_origin: (0, 0),
            black_delta_columns: Vec::new(),
            black_delta_rows: Vec::new(),
            white: vec![white; samples_per_pixel],
            linearization: None,
        }
    }

    fn validate(&self, plane: &RawPlane, area: Area) -> Result<(), DecodeError> {
        let block = &self.black;
        let block_ok = block.width > 0
            && block.height > 0
            && block.values.len() == block.width * block.height
            && block.values.iter().all(|v| v.is_finite());
        let deltas_ok =
            [(&self.black_delta_columns, area.width), (&self.black_delta_rows, area.height)]
                .iter()
                .all(|(deltas, length)| {
                    (deltas.is_empty() || deltas.len() == *length)
                        && deltas.iter().all(|v| v.is_finite())
                });
        if !block_ok || !deltas_ok || self.white.len() != plane.samples_per_pixel {
            return Err(DecodeError::corrupt("raw black or white levels are malformed"));
        }
        let largest = |values: &[f32]| values.iter().copied().fold(0.0_f32, f32::max);
        let black = largest(&block.values)
            + largest(&self.black_delta_columns)
            + largest(&self.black_delta_rows);
        if self.white.iter().any(|&white| !white.is_finite() || white <= black) {
            return Err(DecodeError::corrupt("raw white level is not above black"));
        }
        Ok(())
    }
}

/// The samples inside `area`, linearised and scaled to 0..1 of the white level.
pub fn normalise(plane: &RawPlane, area: Area, levels: &Levels) -> Result<Vec<f32>, DecodeError> {
    let spp = plane.samples_per_pixel;
    let inside = area.width > 0
        && area.height > 0
        && area.left.checked_add(area.width).is_some_and(|right| right <= plane.width)
        && area.top.checked_add(area.height).is_some_and(|bottom| bottom <= plane.height);
    if spp == 0 || !inside || plane.samples.len() != plane.width * plane.height * spp {
        return Err(DecodeError::corrupt("raw image area lies outside the plane"));
    }
    levels.validate(plane, area)?;
    if levels.linearization.as_ref().is_some_and(Vec::is_empty) {
        return Err(DecodeError::corrupt("raw linearisation table is empty"));
    }
    let row_samples = area.width * spp;
    let (origin_column, origin_row) = levels.black_origin;
    let block = &levels.black;
    let mut values = vec![0.0_f32; row_samples * area.height];

    parallel::for_each_indexed_chunk(&mut values, |first, part| {
        for (offset, value) in part.iter_mut().enumerate() {
            let index = first + offset;
            let (row, column) = (index / row_samples, index % row_samples);
            let plane_column = area.left * spp + column;
            let plane_row = area.top + row;
            let raw = plane.samples[plane_row * plane.width * spp + plane_column];
            let linear = match &levels.linearization {
                Some(table) => f32::from(table[usize::from(raw).min(table.len() - 1)]),
                None => f32::from(raw),
            };
            let block_column = plane_column.wrapping_sub(origin_column) % block.width;
            let block_row = plane_row.wrapping_sub(origin_row) % block.height;
            let black = block.values[block_row * block.width + block_column]
                + levels.black_delta_columns.get(column / spp).copied().unwrap_or(0.0)
                + levels.black_delta_rows.get(row).copied().unwrap_or(0.0);
            let white = levels.white[column % spp];
            *value = ((linear - black) / (white - black)).clamp(0.0, 1.0);
        }
    });
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane(width: usize, height: usize, samples: Vec<u16>) -> RawPlane {
        RawPlane { width, height, samples_per_pixel: 1, bits_per_sample: 16, samples }
    }

    #[test]
    fn subtracts_a_black_block_aligned_to_the_plane() {
        let black = BlackLevel { width: 2, height: 1, values: vec![10.0, 20.0] };
        let levels = Levels::simple(black, 110.0, 1);
        let raw = plane(4, 1, vec![60, 65, 110, 20]);
        let area = Area { left: 1, top: 0, width: 3, height: 1 };
        let values = normalise(&raw, area, &levels).unwrap();
        assert_eq!(values, [0.5, 1.0, 0.0]);
    }

    #[test]
    fn rejects_bad_levels_and_areas() {
        let raw = plane(2, 2, vec![0; 4]);
        let full = Area { left: 0, top: 0, width: 2, height: 2 };
        let low_white = Levels::simple(BlackLevel::uniform(50.0), 40.0, 1);
        assert!(normalise(&raw, full, &low_white).is_err());
        let broken = Levels::simple(BlackLevel { width: 2, height: 2, values: vec![0.0] }, 9.0, 1);
        assert!(normalise(&raw, full, &broken).is_err());
        let outside = Area { left: 1, ..full };
        assert!(
            normalise(&raw, outside, &Levels::simple(BlackLevel::uniform(0.0), 9.0, 1)).is_err()
        );
    }
}
