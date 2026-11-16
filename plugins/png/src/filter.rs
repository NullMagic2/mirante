//! Module: mirante_format_png::filter
//! Purpose: The five PNG scanline filters, forwards for writing and inverse for reading.
//! Created: 2026-11-16
//! Architecture: Shared by decode.rs and encode.rs. Rows are raw bytes; `bpp` is
//! the byte distance to the corresponding byte of the previous pixel (at least 1).

use mirante_core::DecodeError;

pub const NONE: u8 = 0;
pub const SUB: u8 = 1;
pub const UP: u8 = 2;
pub const AVERAGE: u8 = 3;
pub const PAETH: u8 = 4;
pub const FILTERS: [u8; 5] = [NONE, SUB, UP, AVERAGE, PAETH];

fn paeth(left: u8, up: u8, up_left: u8) -> u8 {
    let estimate = i16::from(left) + i16::from(up) - i16::from(up_left);
    let distance_left = (estimate - i16::from(left)).abs();
    let distance_up = (estimate - i16::from(up)).abs();
    let distance_up_left = (estimate - i16::from(up_left)).abs();
    if distance_left <= distance_up && distance_left <= distance_up_left {
        left
    } else if distance_up <= distance_up_left {
        up
    } else {
        up_left
    }
}

/// The value a filter predicts for byte `index` of a row from already known bytes.
fn predict(filter: u8, row: &[u8], previous: &[u8], index: usize, bpp: usize) -> u8 {
    let left = if index >= bpp { row[index - bpp] } else { 0 };
    let up = previous[index];
    match filter {
        SUB => left,
        UP => up,
        AVERAGE => ((u16::from(left) + u16::from(up)) / 2) as u8,
        PAETH => paeth(left, up, if index >= bpp { previous[index - bpp] } else { 0 }),
        _ => 0,
    }
}

/// Reverses `filter` in place; `previous` is the decoded row above (zeros for the first).
pub fn unfilter(
    filter: u8,
    row: &mut [u8],
    previous: &[u8],
    bpp: usize,
) -> Result<(), DecodeError> {
    match filter {
        NONE => {}
        UP => row.iter_mut().zip(previous).for_each(|(byte, &up)| *byte = byte.wrapping_add(up)),
        // Specialised loops: this runs for every byte of every decoded image.
        SUB => {
            for index in bpp..row.len() {
                row[index] = row[index].wrapping_add(row[index - bpp]);
            }
        }
        AVERAGE | PAETH => {
            let first = bpp.min(row.len());
            for index in 0..first {
                let up = previous[index];
                row[index] = row[index].wrapping_add(if filter == PAETH { up } else { up / 2 });
            }
            for index in first..row.len() {
                let (left, up) = (row[index - bpp], previous[index]);
                let predicted = if filter == PAETH {
                    paeth(left, up, previous[index - bpp])
                } else {
                    ((u16::from(left) + u16::from(up)) / 2) as u8
                };
                row[index] = row[index].wrapping_add(predicted);
            }
        }
        _ => return Err(DecodeError::corrupt(format!("unknown PNG filter type {filter}"))),
    }
    Ok(())
}

/// Writes `row` filtered with `filter` into `output`, which has the row's length.
pub fn apply(filter: u8, row: &[u8], previous: &[u8], bpp: usize, output: &mut [u8]) {
    for index in 0..row.len() {
        output[index] = row[index].wrapping_sub(predict(filter, row, previous, index, bpp));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_filter_round_trips() {
        let previous = [10, 200, 30, 255, 0, 7];
        let row = [5, 250, 0, 128, 64, 3];
        for filter in FILTERS {
            let mut filtered = [0; 6];
            apply(filter, &row, &previous, 2, &mut filtered);
            unfilter(filter, &mut filtered, &previous, 2).unwrap();
            assert_eq!(filtered, row, "filter {filter}");
        }
        assert!(unfilter(5, &mut [0; 6], &previous, 2).is_err());
    }
}
