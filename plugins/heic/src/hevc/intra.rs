//! Module: mirante_format_heic::hevc::intra
//! Purpose: Intra sample prediction (H.265 8.4.4.2): reference substitution and
//! smoothing, planar, DC and the 33 angular modes with their boundary filters.
//! Created: 2026-11-16
//! Architecture: ctu.rs works out which neighbours are available and calls
//! `predict`, which writes the prediction straight into the picture plane.

use super::picture::PlaneBuffer;

pub const PLANAR: u8 = 0;
pub const DC: u8 = 1;
pub const HORIZONTAL: u8 = 10;
pub const VERTICAL: u8 = 26;
/// Largest block side, so a reference line holds 4 * 32 + 1 samples.
const MAX_SIZE: usize = 32;
const LINE: usize = 4 * MAX_SIZE + 1;

/// intraPredAngle for modes 2..=34 (Table 8-5).
const ANGLES: [i32; 33] = [
    32, 26, 21, 17, 13, 9, 5, 2, 0, -2, -5, -9, -13, -17, -21, -26, -32, -26, -21, -17, -13, -9,
    -5, -2, 0, 2, 5, 9, 13, 17, 21, 26, 32,
];
/// invAngle for modes 11..=25 (Table 8-6).
const INVERSE_ANGLES: [i32; 15] =
    [-4096, -1638, -910, -630, -482, -390, -315, -256, -315, -390, -482, -630, -910, -1638, -4096];

/// How a block is predicted, beyond its mode.
#[derive(Clone, Copy)]
pub struct IntraOptions {
    /// Reference smoothing applies: luma or 4:4:4 chroma, unless disabled by the SPS.
    pub smoothing: bool,
    /// strong_intra_smoothing_enabled_flag, for 32x32 luma.
    pub strong_smoothing: bool,
    /// Luma blocks get the DC and horizontal/vertical edge filters.
    pub edge_filters: bool,
    /// disableIntraBoundaryFilter (implicit RDPCM with transquant bypass).
    pub no_boundary_filter: bool,
}

/// Predicts the block at (x, y) of side `1 << log2_size` into `plane`.
/// `available[i]` follows the reference line: bottom-left upwards, corner, then rightwards.
pub fn predict(
    plane: &mut PlaneBuffer,
    x: usize,
    y: usize,
    log2_size: u32,
    mode: u8,
    available: &[bool],
    options: IntraOptions,
) {
    let size = 1 << log2_size;
    let mut line = [0_i32; LINE];
    let line = &mut line[..4 * size + 1];
    gather_references(plane, x, y, size, available, line);

    let filter = options.smoothing && mode != DC && size != 4 && {
        let distance = (i32::from(mode) - 26).abs().min((i32::from(mode) - 10).abs());
        let threshold = match size {
            8 => 7,
            16 => 1,
            _ => 0,
        };
        distance > threshold
    };
    if filter {
        smooth(line, size, plane.bit_depth, options.strong_smoothing);
    }
    let left = |i: usize| line[2 * size - 1 - i];
    let top = |i: usize| line[2 * size + 1 + i];
    let corner = line[2 * size];
    let stride = plane.width;
    let origin = y * stride + x;
    let max = (1 << plane.bit_depth) - 1;
    let out = &mut plane.samples[origin..];

    match mode {
        PLANAR => {
            let (top_right, bottom_left) = (top(size), left(size));
            for row in 0..size {
                for column in 0..size {
                    let value = (size - 1 - column) as i32 * left(row)
                        + (column + 1) as i32 * top_right
                        + (size - 1 - row) as i32 * top(column)
                        + (row + 1) as i32 * bottom_left
                        + size as i32;
                    out[row * stride + column] = (value >> (log2_size + 1)) as u16;
                }
            }
        }
        DC => {
            let sum: i32 = (0..size).map(|i| top(i) + left(i)).sum::<i32>() + size as i32;
            let dc = sum >> (log2_size + 1);
            for row in 0..size {
                out[row * stride..row * stride + size].fill(dc as u16);
            }
            if options.edge_filters && size < MAX_SIZE {
                out[0] = ((left(0) + 2 * dc + top(0) + 2) >> 2) as u16;
                for i in 1..size {
                    out[i] = ((top(i) + 3 * dc + 2) >> 2) as u16;
                    out[i * stride] = ((left(i) + 3 * dc + 2) >> 2) as u16;
                }
            }
        }
        _ => {
            let angle = ANGLES[usize::from(mode) - 2];
            let vertical = mode >= 18;
            // The main reference runs along the predicted direction, ref[0] at the corner.
            let mut reference = [0_i32; 3 * MAX_SIZE + 1];
            let base = MAX_SIZE;
            let (main, side): (&dyn Fn(usize) -> i32, &dyn Fn(usize) -> i32) =
                if vertical { (&top, &left) } else { (&left, &top) };
            reference[base] = corner;
            for i in 0..size {
                reference[base + 1 + i] = main(i);
            }
            if angle < 0 {
                let last = (size as i32 * angle) >> 5;
                if last < -1 {
                    let inverse = INVERSE_ANGLES[usize::from(mode) - 11];
                    for k in last..=-1 {
                        let index = ((k * inverse + 128) >> 8) - 1;
                        reference[(base as i32 + k) as usize] = side(index as usize);
                    }
                }
            } else {
                for i in size..2 * size {
                    reference[base + 1 + i] = main(i);
                }
            }
            for j in 0..size {
                let position = (j as i32 + 1) * angle;
                let (index, fraction) = (position >> 5, position & 31);
                for i in 0..size {
                    let at = (base as i32 + i as i32 + index + 1) as usize;
                    let value = if fraction == 0 {
                        reference[at]
                    } else {
                        ((32 - fraction) * reference[at] + fraction * reference[at + 1] + 16) >> 5
                    };
                    let (row, column) = if vertical { (j, i) } else { (i, j) };
                    out[row * stride + column] = value as u16;
                }
            }
            let boundary = options.edge_filters && !options.no_boundary_filter && size < MAX_SIZE;
            if boundary && angle == 0 {
                for i in 0..size {
                    let value = (main(0) + ((side(i) - corner) >> 1)).clamp(0, max);
                    let at = if vertical { i * stride } else { i };
                    out[at] = value as u16;
                }
            }
        }
    }
}

/// 8.4.4.2.2: reads the neighbours and substitutes the unavailable ones.
fn gather_references(
    plane: &PlaneBuffer,
    x: usize,
    y: usize,
    size: usize,
    available: &[bool],
    line: &mut [i32],
) {
    let stride = plane.width;
    let Some(first) = available.iter().position(|&a| a) else {
        line.fill(1 << (plane.bit_depth - 1));
        return;
    };
    for (i, value) in line.iter_mut().enumerate() {
        if !available[i] {
            continue;
        }
        let (sx, sy) = match i.cmp(&(2 * size)) {
            std::cmp::Ordering::Less => (x - 1, y + 2 * size - 1 - i),
            std::cmp::Ordering::Equal => (x - 1, y - 1),
            std::cmp::Ordering::Greater => (x + i - 2 * size - 1, y - 1),
        };
        *value = i32::from(plane.samples[sy * stride + sx]);
    }
    let mut previous = line[first];
    for (i, value) in line.iter_mut().enumerate() {
        if available[i] {
            previous = *value;
        } else {
            *value = previous;
        }
    }
}

/// 8.4.4.2.3: [1 2 1] smoothing, or bilinear interpolation for flat 32x32 luma.
fn smooth(line: &mut [i32], size: usize, bit_depth: u32, strong_enabled: bool) {
    let last = 4 * size;
    let (bottom, corner, right) = (line[0], line[2 * size], line[last]);
    let flat = |a: i32, b: i32, c: i32| (a + c - 2 * b).abs() < (1 << (bit_depth - 5));
    if strong_enabled
        && size == MAX_SIZE
        && flat(corner, line[2 * size + size], right)
        && flat(corner, line[size], bottom)
    {
        for i in 0..2 * size - 1 {
            let k = i as i32;
            line[2 * size - 1 - i] = ((63 - k) * corner + (k + 1) * bottom + 32) >> 6;
            line[2 * size + 1 + i] = ((63 - k) * corner + (k + 1) * right + 32) >> 6;
        }
        return;
    }
    let mut previous = line[0];
    for i in 1..last {
        let current = line[i];
        line[i] = (previous + 2 * current + line[i + 1] + 2) >> 2;
        previous = current;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPTIONS: IntraOptions = IntraOptions {
        smoothing: true,
        strong_smoothing: false,
        edge_filters: true,
        no_boundary_filter: false,
    };

    fn plane() -> PlaneBuffer {
        PlaneBuffer { width: 8, height: 8, bit_depth: 8, samples: vec![0; 64] }
    }

    #[test]
    fn without_neighbours_predicts_mid_grey() {
        let mut plane = plane();
        predict(&mut plane, 0, 0, 2, PLANAR, &[false; 17], OPTIONS);
        assert!(plane.samples[..4].iter().all(|&s| s == 128));
    }

    #[test]
    fn vertical_mode_copies_the_row_above() {
        let mut plane = plane();
        for x in 0..8 {
            plane.samples[3 * 8 + x] = 10 * x as u16;
        }
        let mut available = [false; 17];
        available[9..].fill(true);
        predict(&mut plane, 0, 4, 2, VERTICAL, &available, OPTIONS);
        // Column 0 gets the boundary filter, which here leaves it unchanged.
        assert_eq!(plane.samples[7 * 8..7 * 8 + 4], [0, 10, 20, 30]);
    }
}
