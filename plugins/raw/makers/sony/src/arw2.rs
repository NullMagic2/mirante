//! Module: mirante_raw_sony::arw2
//! Purpose: Unpacks Sony's compressed ARW 2 raw data and its tone curve.
//! Created: 2026-11-16
//! Architecture: Ported from dcraw by Dave Coffin (sony_arw2_load_raw). Each row
//! holds one byte per photosite in 16-byte blocks of 16 same-colour photosites:
//! an 11-bit maximum and minimum, their positions, and 7-bit scaled offsets.

use mirante_core::DecodeError;

const BLOCK_BYTES: usize = 16;
const BLOCK_PIXELS: usize = 16;
/// A span of 32 columns holds one block of even and one of odd columns.
const SPAN_COLUMNS: usize = 2 * BLOCK_PIXELS;
const CURVE_LENGTH: usize = 0x1000;
const ELEVEN_BITS: u32 = 0x7ff;

/// Maps the 12-bit coded values back to linear sensor values.
pub struct ToneCurve(Vec<u16>);

impl ToneCurve {
    /// Builds the curve from tag 0x7010's four knee points (14-bit values).
    /// Segment `i` between knees rises by 2^i per step.
    pub fn from_knees(knees: &[u32]) -> Self {
        let mut curve: Vec<u16> = (0..CURVE_LENGTH as u16).collect();
        let mut points = [0_usize, 0, 0, 0, 0, CURVE_LENGTH - 1];
        for (point, &knee) in points[1..5].iter_mut().zip(knees) {
            *point = (knee >> 2 & 0xfff) as usize;
        }
        for segment in 0..5 {
            for j in points[segment] + 1..=points[segment + 1] {
                curve[j] = curve[j - 1].saturating_add(1 << segment);
            }
        }
        Self(curve)
    }

    pub fn identity() -> Self {
        Self((0..CURVE_LENGTH as u16).collect())
    }
}

/// Decodes `height` rows of `width` photosites; `width` must be a multiple of 32.
pub fn decode(
    data: &[u8],
    width: usize,
    height: usize,
    curve: &ToneCurve,
) -> Result<Vec<u16>, DecodeError> {
    if width % SPAN_COLUMNS != 0 {
        return Err(DecodeError::unsupported(format!("ARW 2 rows of {width} photosites")));
    }
    if data.len() < width * height {
        return Err(DecodeError::corrupt("ARW raw data is truncated"));
    }
    let mut samples = vec![0; width * height];
    for (row, bytes) in samples.chunks_exact_mut(width).zip(data.chunks_exact(width)) {
        for (span_index, span) in row.chunks_exact_mut(SPAN_COLUMNS).enumerate() {
            for parity in 0..2 {
                let start = span_index * SPAN_COLUMNS + parity * BLOCK_BYTES;
                let pixels = decode_block(&bytes[start..]);
                for (i, value) in pixels.into_iter().enumerate() {
                    // LibRaw keeps the curve's 14-bit scale but drops its two low bits.
                    span[2 * i + parity] = curve.0[usize::from(value) << 1] & !3;
                }
            }
        }
    }
    Ok(samples)
}

/// Decodes the block at the start of `bytes` into 16 coded 11-bit values. When
/// the maximum and minimum share a position, a 15th delta reaches into the next
/// block, as in dcraw; past the row's end that reads zeros.
pub fn decode_block(bytes: &[u8]) -> [u16; BLOCK_PIXELS] {
    let mut block = [0_u8; BLOCK_BYTES + 2];
    let available = bytes.len().min(block.len());
    block[..available].copy_from_slice(&bytes[..available]);
    let header = u32::from_le_bytes([block[0], block[1], block[2], block[3]]);
    let max = header & ELEVEN_BITS;
    let min = header >> 11 & ELEVEN_BITS;
    let max_index = (header >> 22 & 0xf) as usize;
    let min_index = (header >> 26 & 0xf) as usize;
    let mut shift = 0;
    while shift < 4 && 0x80 << shift <= max.saturating_sub(min) {
        shift += 1;
    }
    let mut position = 30;
    let mut pixels = [0; BLOCK_PIXELS];
    for (i, pixel) in pixels.iter_mut().enumerate() {
        *pixel = if i == max_index {
            max
        } else if i == min_index {
            min
        } else {
            let pair = u16::from_le_bytes([block[position / 8], block[position / 8 + 1]]);
            let delta = u32::from(pair >> (position % 8)) & 0x7f;
            position += 7;
            ((delta << shift) + min).min(ELEVEN_BITS)
        } as u16;
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Packs a block the way the camera does.
    fn block(max: u32, min: u32, max_index: u32, min_index: u32, deltas: &[u32]) -> [u8; 16] {
        let mut bits = u128::from(max | min << 11 | max_index << 22 | min_index << 26);
        for (i, &delta) in deltas.iter().enumerate() {
            bits |= u128::from(delta) << (30 + 7 * i);
        }
        bits.to_le_bytes()
    }

    #[test]
    fn decodes_maximum_minimum_and_scaled_deltas() {
        let deltas: Vec<u32> = (0..14).map(|i| i * 9).collect();
        // max - min = 300 needs a shift of 2 so that 7-bit deltas cover it.
        let pixels = decode_block(&block(400, 100, 3, 0, &deltas));
        assert_eq!(pixels[0], 100);
        assert_eq!(pixels[3], 400);
        assert_eq!(pixels[1], 100);
        assert_eq!(pixels[2], 100 + (9 << 2));
        assert_eq!(pixels[4], 100 + (18 << 2));
        assert_eq!(pixels[15], 100 + ((13 * 9) << 2));
    }

    #[test]
    fn shared_position_reads_a_fifteenth_delta_from_the_next_block() {
        let mut bytes = block(100, 100, 0, 0, &[1; 14]).to_vec();
        bytes.extend([0x55, 0]);
        let pixels = decode_block(&bytes);
        assert_eq!((pixels[0], pixels[1], pixels[15]), (100, 101, 100 + 0x55));
        // At the end of a row the missing bytes read as zeros.
        assert_eq!(decode_block(&bytes[..16])[15], 100);
    }

    #[test]
    fn interleaves_even_and_odd_blocks() {
        let even = block(20, 10, 0, 1, &[0; 14]);
        let odd = block(50, 40, 1, 0, &[0; 14]);
        let data: Vec<u8> = [even, odd].concat();
        let samples = decode(&data, 32, 1, &ToneCurve::identity()).unwrap();
        // The curve is indexed at twice the coded value.
        assert_eq!(&samples[..4], &[40, 80, 20, 100]);
    }

    #[test]
    fn tone_curve_doubles_its_slope_at_each_knee() {
        let curve = ToneCurve::from_knees(&[40, 80, 120, 160]);
        // Knees at coded 10, 20, 30, 40.
        assert_eq!(curve.0[10], 10);
        assert_eq!(curve.0[12], 14);
        assert_eq!(curve.0[21], 34);
        assert_eq!(curve.0[30], 70);
        assert_eq!(curve.0[4095], 150 + (4095 - 40) * 16);
    }

    #[test]
    fn rejects_truncated_rows() {
        assert!(decode(&[0; 31], 32, 1, &ToneCurve::identity()).is_err());
    }
}
