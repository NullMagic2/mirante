//! Module: mirante_raw_panasonic::blocks
//! Purpose: Decodes Panasonic's block-compressed raw data (RW2 formats 1 to 4).
//! Created: 2026-11-16
//! Architecture: Ported from dcraw by Dave Coffin (pana_bits, panasonic_load_raw).
//! Data comes in 0x4000-byte blocks stored rotated at a split point; each block
//! is read as 16-byte little-endian groups, high bits first, 14 photosites each.

use mirante_core::DecodeError;

const BLOCK_BYTES: usize = 0x4000;
/// Bits per block; the bit position counts down modulo this.
const BLOCK_BITS_MASK: u32 = 0x1ffff;
const GROUP_PIXELS: usize = 14;
const GROUP_BYTES: usize = 16;

/// dcraw's pana_bits: a bit position counting down through a rotated block.
struct PanaBits<'a> {
    data: &'a [u8],
    next_block: usize,
    split: usize,
    /// One spare byte, as reads look one byte ahead.
    buffer: Box<[u8; BLOCK_BYTES + 1]>,
    position: u32,
}

impl<'a> PanaBits<'a> {
    fn new(data: &'a [u8], split: usize) -> Self {
        let buffer = Box::new([0; BLOCK_BYTES + 1]);
        Self { data, next_block: 0, split, buffer, position: 0 }
    }

    fn bits(&mut self, count: u32) -> Result<u32, DecodeError> {
        if self.position == 0 {
            self.load_block()?;
        }
        self.position = self.position.wrapping_sub(count) & BLOCK_BITS_MASK;
        let byte = (self.position >> 3 ^ 0x3ff0) as usize;
        let pair = u32::from(self.buffer[byte]) | u32::from(self.buffer[byte + 1]) << 8;
        Ok(pair >> (self.position & 7) & ((1 << count) - 1))
    }

    fn load_block(&mut self) -> Result<(), DecodeError> {
        let start = self.next_block * BLOCK_BYTES;
        let block = self
            .data
            .get(start..start + BLOCK_BYTES)
            .ok_or_else(|| DecodeError::corrupt("Panasonic raw data is truncated"))?;
        let head = BLOCK_BYTES - self.split;
        self.buffer[self.split..BLOCK_BYTES].copy_from_slice(&block[..head]);
        self.buffer[..self.split].copy_from_slice(&block[head..]);
        self.next_block += 1;
        Ok(())
    }
}

/// Decodes `height` rows of `width` photosites; `split` is 0x2008 for RW2.
pub fn decode(
    data: &[u8],
    width: usize,
    height: usize,
    split: usize,
) -> Result<Vec<u16>, DecodeError> {
    let pixels = width * height;
    if split > BLOCK_BYTES || data.len() < pixels / GROUP_PIXELS * GROUP_BYTES {
        return Err(DecodeError::corrupt("Panasonic raw data is truncated"));
    }
    let mut bits = PanaBits::new(data, split);
    let mut samples = vec![0_u16; pixels];
    let mut pred = [0_i32; 2];
    let mut nonzero = [0_i32; 2];
    let mut shift = 0;
    for row in samples.chunks_exact_mut(width) {
        for (col, sample) in row.iter_mut().enumerate() {
            let i = col % GROUP_PIXELS;
            if i == 0 {
                pred = [0; 2];
                nonzero = [0; 2];
            }
            if i % 3 == 2 {
                shift = 4 >> (3 - bits.bits(2)?);
            }
            let k = i & 1;
            if nonzero[k] != 0 {
                let step = bits.bits(8)? as i32;
                if step != 0 {
                    pred[k] -= 0x80 << shift;
                    if pred[k] < 0 || shift == 4 {
                        pred[k] &= !(-1 << shift);
                    }
                    pred[k] += step << shift;
                }
            } else {
                nonzero[k] = bits.bits(8)? as i32;
                if nonzero[k] != 0 || i > 11 {
                    pred[k] = nonzero[k] << 4 | bits.bits(4)? as i32;
                }
            }
            // Like dcraw, the photosite keeps the low 16 bits.
            *sample = pred[col & 1] as u16;
        }
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Packs fields high bits first into one 16-byte group.
    fn group(fields: &[(u32, u32)]) -> [u8; GROUP_BYTES] {
        let mut value = 0_u128;
        let mut used = 0;
        for &(field, count) in fields {
            used += count;
            value |= u128::from(field) << (128 - used);
        }
        assert_eq!(used, 128);
        value.to_le_bytes()
    }

    #[test]
    fn decodes_a_group_from_a_rotated_block() {
        // Pixels 0 and 1 start at 1000 and 600; at pixel 2 a shift of 4 and a
        // step of 0x90 rebase the even channel.
        let mut fields = vec![(1000 >> 4, 8), (1000 & 15, 4), (600 >> 4, 8), (600 & 15, 4)];
        fields.extend([(3, 2), (0x90, 8), (0, 8), (0, 8), (1, 2), (0, 8)]);
        fields.extend([
            (0, 8),
            (0, 8),
            (1, 2),
            (0, 8),
            (0, 8),
            (0, 8),
            (1, 2),
            (0, 8),
            (0, 8),
            (0, 8),
        ]);
        let mut block = vec![0; BLOCK_BYTES];
        // The first 0x2008 bytes of the buffer come from the end of the stored block.
        let stored_at = BLOCK_BYTES - 0x2008;
        block[stored_at..stored_at + GROUP_BYTES].copy_from_slice(&group(&fields));
        let samples = decode(&block, 14, 1, 0x2008).unwrap();
        let rebased = (((1000 - 2048) & 15) + (0x90 << 4)) as u16;
        assert_eq!(&samples[..4], &[1000, 600, rebased, 600]);
        assert_eq!(samples[12], rebased);
    }

    #[test]
    fn reports_truncated_data() {
        assert!(decode(&[0; 100], 14, 14, 0x2008).is_err());
    }
}
