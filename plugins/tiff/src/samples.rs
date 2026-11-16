//! Module: mirante_format_tiff::samples
//! Purpose: Turns one stored row of bytes into sample values, undoing predictors.
//! Created: 2026-11-16
//! Architecture: Integers keep their stored value (the caller normalises them);
//! floats come out as their value. Both predictors work on whole stored rows.

use mirante_codec::Endian;

pub const PREDICTOR_NONE: u16 = 1;
pub const PREDICTOR_HORIZONTAL: u16 = 2;
pub const PREDICTOR_FLOAT: u16 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleFormat {
    Unsigned,
    Float,
}

#[derive(Clone, Copy, Debug)]
pub struct RowFormat {
    pub bits: u32,
    pub format: SampleFormat,
    pub predictor: u16,
    pub endian: Endian,
    /// Samples per pixel within this row, the predictor's step.
    pub stride: usize,
}

impl RowFormat {
    pub fn row_bytes(&self, samples: usize) -> usize {
        (samples * self.bits as usize).div_ceil(8)
    }
}

/// Decodes `count` samples; `row` holds at least `format.row_bytes(count)` bytes.
pub fn decode_row(row: &[u8], count: usize, format: &RowFormat) -> Vec<f32> {
    let mut values = if format.predictor == PREDICTOR_FLOAT {
        let bytes = undo_float_predictor(row, count, format);
        unpack(&bytes, count, format.bits, Endian::Big)
    } else {
        unpack(row, count, format.bits, format.endian)
    };
    if format.predictor == PREDICTOR_HORIZONTAL {
        let mask = if format.bits == 32 { u32::MAX } else { (1 << format.bits) - 1 };
        for index in format.stride..values.len() {
            values[index] = values[index].wrapping_add(values[index - format.stride]) & mask;
        }
    }
    match (format.format, format.bits) {
        (SampleFormat::Float, 16) => values.iter().map(|&v| half_to_f32(v as u16)).collect(),
        (SampleFormat::Float, _) => values.iter().map(|&v| f32::from_bits(v)).collect(),
        (SampleFormat::Unsigned, _) => values.iter().map(|&v| v as f32).collect(),
    }
}

fn unpack(row: &[u8], count: usize, bits: u32, endian: Endian) -> Vec<u32> {
    let read = |pair: &[u8]| match (pair, endian) {
        ([a, b], Endian::Big) => u32::from(u16::from_be_bytes([*a, *b])),
        ([a, b], Endian::Little) => u32::from(u16::from_le_bytes([*a, *b])),
        ([a, b, c, d], Endian::Big) => u32::from_be_bytes([*a, *b, *c, *d]),
        ([a, b, c, d], Endian::Little) => u32::from_le_bytes([*a, *b, *c, *d]),
        _ => unreachable!("chunks have the sample size"),
    };
    match bits {
        8 => row[..count].iter().map(|&b| u32::from(b)).collect(),
        16 | 32 => row.chunks_exact(bits as usize / 8).take(count).map(read).collect(),
        _ => {
            let mask = (1_u32 << bits) - 1;
            (0..count)
                .map(|index| {
                    let bit = index * bits as usize;
                    let shift = 8 - bits as usize - bit % 8;
                    (u32::from(row[bit / 8]) >> shift) & mask
                })
                .collect()
        }
    }
}

/// Predictor 3 differences bytes, then stores each byte plane of the row in
/// turn, most significant first; this returns big-endian samples.
fn undo_float_predictor(row: &[u8], count: usize, format: &RowFormat) -> Vec<u8> {
    let width = format.bits as usize / 8;
    let mut planes = row[..count * width].to_vec();
    for index in format.stride..planes.len() {
        planes[index] = planes[index].wrapping_add(planes[index - format.stride]);
    }
    let mut samples = vec![0; planes.len()];
    for (index, sample) in samples.chunks_exact_mut(width).enumerate() {
        for (byte, value) in sample.iter_mut().enumerate() {
            *value = planes[byte * count + index];
        }
    }
    samples
}

/// IEEE 754 binary16 to binary32.
pub fn half_to_f32(half: u16) -> f32 {
    /// 2^-24, the value of the smallest subnormal half.
    const SUBNORMAL_STEP: f32 = 5.960_464_5e-8;
    let exponent = u32::from((half >> 10) & 0x1F);
    let mantissa = u32::from(half & 0x3FF);
    let magnitude = match exponent {
        0 => mantissa as f32 * SUBNORMAL_STEP,
        0x1F if mantissa == 0 => f32::INFINITY,
        0x1F => f32::NAN,
        _ => f32::from_bits(((exponent + 112) << 23) | (mantissa << 13)),
    };
    if half & 0x8000 == 0 { magnitude } else { -magnitude }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(bits: u32, format: SampleFormat, predictor: u16, stride: usize) -> RowFormat {
        RowFormat { bits, format, predictor, endian: Endian::Little, stride }
    }

    #[test]
    fn unpacks_sub_byte_samples_msb_first() {
        let row = decode_row(&[0b1011_0001], 4, &format(2, SampleFormat::Unsigned, 1, 1));
        assert_eq!(row, [2.0, 3.0, 0.0, 1.0]);
    }

    #[test]
    fn undoes_horizontal_differencing_per_channel() {
        // Stored differences 447, -200 and 0 follow the first pixel (10, 200, 257).
        let row = [10, 0, 200, 0, 1, 1, 191, 1, 0x38, 0xFF, 0, 0];
        let row = decode_row(&row, 6, &format(16, SampleFormat::Unsigned, 2, 3));
        assert_eq!(row, [10.0, 200.0, 257.0, 457.0, 0.0, 257.0]);
    }

    #[test]
    fn undoes_floating_point_predictor() {
        // Two 32-bit floats 1.0 (3F800000) and 2.0 (40000000), byte planes differenced.
        let planes: [u8; 8] = [0x3F, 0x40, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00];
        let mut row = planes;
        for index in (1..row.len()).rev() {
            row[index] = row[index].wrapping_sub(row[index - 1]);
        }
        let row = decode_row(&row, 2, &format(32, SampleFormat::Float, 3, 1));
        assert_eq!(row, [1.0, 2.0]);
    }

    #[test]
    fn converts_half_floats() {
        assert_eq!(half_to_f32(0x3C00), 1.0);
        assert_eq!(half_to_f32(0xC000), -2.0);
        assert_eq!(half_to_f32(0x0001), 5.960_464_5e-8);
    }
}
