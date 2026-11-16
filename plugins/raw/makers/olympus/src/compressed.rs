//! Module: mirante_raw_olympus::compressed
//! Purpose: Decodes Olympus' 12-bit lossless raw compression.
//! Created: 2026-11-16
//! Architecture: Ported from dcraw by Dave Coffin (olympus_load_raw). Each colour
//! of a row pair keeps an adaptive bit-length state; samples are predicted from
//! the same-colour neighbours two columns left and two rows up.

use mirante_core::DecodeError;
use mirante_raw::bits::BitReader;

/// The stream starts after a 7-byte header.
const HEADER_BYTES: usize = 7;
/// Longest run of leading zeros the length code spells out.
const MAX_HIGH: u32 = 12;

/// Counts leading zeros up to MAX_HIGH, consuming them and the one bit after.
fn leading_zeros(pump: &mut BitReader<'_>) -> u32 {
    let window = pump.peek(MAX_HIGH);
    let zeros = (window << (32 - MAX_HIGH)).leading_zeros().min(MAX_HIGH);
    pump.consume((zeros + 1).min(MAX_HIGH));
    zeros
}

/// Decodes `height` rows of `width` 12-bit samples starting at the strip offset.
pub fn decode(data: &[u8], width: usize, height: usize) -> Result<Vec<u16>, DecodeError> {
    let stream = data.get(HEADER_BYTES..).ok_or_else(truncated)?;
    let mut pump = BitReader::new(stream);
    let mut samples = vec![0_u16; width * height];
    for row in 0..height {
        let mut carries = [[0_i32; 3]; 2];
        for col in 0..width {
            let carry = &mut carries[col & 1];
            let (diff, low) = next_difference(&mut pump, carry);
            let at = |r: usize, c: usize| i32::from(samples[r * width + c]);
            let pred = match (row < 2, col < 2) {
                (true, true) => 0,
                (true, false) => at(row, col - 2),
                (false, true) => at(row - 2, col),
                (false, false) => predict(at(row, col - 2), at(row - 2, col), at(row - 2, col - 2)),
            };
            // Like dcraw, the stored photosite keeps the low 16 bits.
            samples[row * width + col] = pred.wrapping_add((diff << 2) | low) as u16;
        }
        if pump.overrun() {
            return Err(truncated());
        }
    }
    Ok(samples)
}

/// Reads one coded difference and its two separately stored low bits.
fn next_difference(pump: &mut BitReader<'_>, carry: &mut [i32; 3]) -> (i32, i32) {
    let i = if carry[2] < 3 { 2 } else { 0 };
    let mut nbits = 2 + i;
    while u32::from(carry[0] as u16) >> (nbits + i) != 0 {
        nbits += 1;
    }
    let sign_and_low = pump.read(3);
    let low = (sign_and_low & 3) as i32;
    let sign = if sign_and_low & 4 != 0 { -1 } else { 0 };
    let mut high = leading_zeros(pump);
    if high == MAX_HIGH {
        high = pump.read(16 - nbits) >> 1;
    }
    carry[0] = ((high << nbits) | pump.read(nbits)) as i32;
    let diff = (carry[0] ^ sign).wrapping_add(carry[1]);
    carry[1] = (diff.wrapping_mul(3).wrapping_add(carry[1])) >> 5;
    carry[2] = if carry[0] > 16 { 0 } else { carry[2] + 1 };
    (diff, low)
}

/// Median-edge style predictor from the west, north and north-west neighbours.
fn predict(w: i32, n: i32, nw: i32) -> i32 {
    if (w < nw && nw < n) || (n < nw && nw < w) {
        if (w - nw).abs() > 32 || (n - nw).abs() > 32 { w + n - nw } else { (w + n) >> 1 }
    } else if (w - nw).abs() > (n - nw).abs() {
        w
    } else {
        n
    }
}

fn truncated() -> DecodeError {
    DecodeError::corrupt("Olympus raw data is truncated")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes bits most significant first.
    #[derive(Default)]
    struct BitWriter {
        bytes: Vec<u8>,
        bits: u32,
    }

    impl BitWriter {
        fn put(&mut self, value: u32, count: u32) {
            for bit in (0..count).rev() {
                if self.bits % 8 == 0 {
                    self.bytes.push(0);
                }
                let last = self.bytes.len() - 1;
                self.bytes[last] |= ((value >> bit & 1) as u8) << (7 - self.bits % 8);
                self.bits += 1;
            }
        }
    }

    #[test]
    fn predictor_follows_smooth_gradients_and_edges() {
        assert_eq!(predict(100, 120, 110), 110);
        assert_eq!(predict(100, 200, 150), 150);
        assert_eq!(predict(100, 300, 150), 250);
        assert_eq!(predict(100, 130, 300), 100);
    }

    #[test]
    fn decodes_first_samples_of_a_row() {
        // State starts at nbits 4 (carry[2] < 3 adds two): code "sign 0, low 1",
        // one leading zero, then four bits 0b0101 give carry 0b1_0101 = 21.
        let mut writer = BitWriter::default();
        writer.put(0b001, 3);
        writer.put(0b01, 2);
        writer.put(0b0101, 4);
        let mut data = vec![0; HEADER_BYTES];
        data.extend(&writer.bytes);
        data.extend([0; 8]);
        let samples = decode(&data, 1, 1).unwrap();
        assert_eq!(samples, [21 << 2 | 1]);
    }

    #[test]
    fn reports_truncation() {
        assert!(decode(&[0; HEADER_BYTES + 2], 64, 2).is_err());
    }
}
