//! Module: mirante_raw_canon::crx::bits
//! Purpose: The CRX bitstream: MSB-first bits, unary zero runs, adaptive
//! Golomb-Rice codes and the run-length mode shared by every CRX line coder.
//! Created: 2026-11-16
//! Architecture: Built on mirante_raw::bits. Reading past the end yields zero
//! bits, like Canon's decoder; only a zero run that never ends is an error. Based on Alexey Danilchenko's
//! CRX description as implemented in LibRaw (crx.cpp).

use mirante_core::DecodeError;
use mirante_raw::bits;

/// Zero runs this long or longer escape to a fixed-width value.
const ESCAPE_ZEROS: u32 = 41;
const ESCAPE_BITS: u32 = 21;
/// Golomb parameters beyond this cannot come from valid data.
const MAX_K: u32 = 31;
/// Run lengths grow as powers of two (`1 << RUN_BITS[s]`), like JPEG-LS.
const RUN_BITS: [u32; 32] = [
    0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 9, 10, 11, 12, 13,
    14, 15,
];

/// Zero bits examined per step while counting a run.
const WINDOW: u32 = 32;

/// The shared MSB-first reader plus CRX's codes.
pub struct BitReader<'a> {
    inner: bits::BitReader<'a>,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { inner: bits::BitReader::new(data) }
    }

    /// The next `count` (at most 32) bits; zeros once the data runs out.
    pub fn bits(&mut self, count: u32) -> u32 {
        self.inner.read(count)
    }

    /// Counts zero bits up to the next one bit, which is consumed too.
    pub fn zeros(&mut self) -> Result<u32, DecodeError> {
        let mut count = 0;
        loop {
            let window = self.inner.peek(WINDOW);
            if window != 0 {
                let leading = window.leading_zeros();
                self.inner.consume(leading + 1);
                return Ok(count + leading);
            }
            if self.inner.overrun() {
                return Err(DecodeError::corrupt("CRX bitstream ends inside a code"));
            }
            count += WINDOW;
            self.inner.consume(WINDOW);
        }
    }

    /// An adaptive Golomb-Rice code with parameter `k`.
    pub fn code(&mut self, k: u32) -> Result<u32, DecodeError> {
        let zeros = self.zeros()?;
        if zeros >= ESCAPE_ZEROS {
            return Ok(self.bits(ESCAPE_BITS));
        }
        if k == 0 {
            return Ok(zeros);
        }
        if k > MAX_K {
            return Err(DecodeError::corrupt("CRX Golomb parameter out of range"));
        }
        Ok((zeros << k) | self.bits(k))
    }

    /// Length of a run of repeated symbols, at most `remaining`; `state` adapts
    /// the run granularity between runs.
    pub fn run(&mut self, remaining: usize, state: &mut usize) -> Result<usize, DecodeError> {
        if self.bits(1) == 0 {
            return Ok(0);
        }
        let mut length = 1;
        while self.bits(1) == 1 {
            length += 1 << RUN_BITS[*state];
            if length > remaining {
                length = remaining;
                break;
            }
            *state = (*state + 1).min(RUN_BITS.len() - 1);
            if length == remaining {
                break;
            }
        }
        if length < remaining {
            length += self.bits(RUN_BITS[*state]) as usize;
            *state = state.saturating_sub(1);
            if length > remaining {
                return Err(DecodeError::corrupt("CRX run passes the end of the line"));
            }
        }
        Ok(length)
    }
}

/// Maps 0, 1, 2, 3, ... to 0, -1, 1, -2, ...
pub fn signed(code: u32) -> i32 {
    -((code & 1) as i32) ^ (code >> 1) as i32
}

/// Adapts the Golomb parameter to the last code; `limit` 0 means unbounded.
pub fn next_k(k: u32, code: u32, limit: u32) -> u32 {
    let code = u64::from(code);
    let scaled = code >> k.min(63);
    let smaller = code < (1_u64 << k.min(63)) >> 1;
    let next = k - u32::from(smaller) + u32::from(scaled > 2) + u32::from(scaled > 5);
    if limit == 0 || next < limit { next } else { limit }
}

/// LOCO-I median edge predictor, in Canon's formulation.
pub fn median(left: i32, top: i32, top_left: i32) -> i32 {
    let delta = top.wrapping_sub(top_left);
    let gradient = left.wrapping_add(delta);
    let choices = [gradient, gradient, left, top];
    let flip = delta < 0;
    choices[(usize::from((top_left < left) ^ flip) << 1) + usize::from((left < top) ^ flip)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_bits_zero_runs_and_codes() {
        // 101, 0001, then ten zeros and a one.
        let mut reader = BitReader::new(&[0b1010_0010, 0b0000_0000, 0b0100_0000]);
        assert_eq!(reader.bits(3), 0b101);
        assert_eq!(reader.zeros().unwrap(), 3);
        assert_eq!(reader.zeros().unwrap(), 10);
        assert_eq!(reader.bits(8), 0);
        assert!(reader.zeros().is_err());
        // Zero run 2, then k = 3 bits 101: (2 << 3) | 5.
        assert_eq!(BitReader::new(&[0b0011_0100]).code(3).unwrap(), 21);
    }

    #[test]
    fn decodes_runs_with_adaptive_granularity() {
        // Start bit, two continuation bits, stop bit: 1 + 1 + 1 at state 0, then
        // the state has grown to 2 and its remainder has no bits.
        let mut state = 0;
        let mut reader = BitReader::new(&[0b1110_0000]);
        assert_eq!(reader.run(10, &mut state).unwrap(), 3);
        assert_eq!(state, 1);
        // A run is clipped to the end of the line.
        let mut reader = BitReader::new(&[0xFF, 0xFF]);
        assert_eq!(reader.run(4, &mut 0).unwrap(), 4);
    }

    #[test]
    fn signed_codes_and_predictors() {
        assert_eq!([0, 1, 2, 3, 4].map(signed), [0, -1, 1, -2, 2]);
        assert_eq!(next_k(0, 0, 15), 0);
        assert_eq!(next_k(2, 0, 15), 1);
        assert_eq!(next_k(2, 24, 15), 4);
        assert_eq!(next_k(15, 1 << 20, 15), 15);
        assert_eq!(median(10, 20, 5), 20);
        assert_eq!(median(10, 20, 30), 10);
        assert_eq!(median(10, 20, 15), 15);
    }
}
