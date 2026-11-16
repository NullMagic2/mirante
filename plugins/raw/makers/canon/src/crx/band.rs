//! Module: mirante_raw_canon::crx::band
//! Purpose: Decodes one CRX subband line by line.
//! Created: 2026-11-16
//! Architecture: Lossless planes and wavelet low-pass bands predict each value
//! from its neighbours (median predictor, `Predicted`); wavelet detail bands
//! code values directly with a per-column Golomb context (`Unpredicted`).
//! Lines keep one spare value on each side, so column `c` sits at index `c + 1`.
//! Ported from LibRaw's crx.cpp (Alexey Danilchenko), credited in crx/mod.rs.

use super::bits::{BitReader, median, next_k, signed};
use mirante_core::DecodeError;

const K_LIMIT: u32 = 15;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coding {
    Predicted,
    Unpredicted,
}

pub struct BandDecoder<'a> {
    bits: BitReader<'a>,
    coding: Coding,
    width: usize,
    line: usize,
    previous: Vec<i32>,
    current: Vec<i32>,
    /// Golomb parameter each column of the previous line ended with.
    column_k: Vec<u32>,
    k: u32,
    run_state: usize,
}

impl<'a> BandDecoder<'a> {
    pub fn new(data: &'a [u8], width: usize, coding: Coding) -> Self {
        Self {
            bits: BitReader::new(data),
            coding,
            width,
            line: 0,
            previous: vec![0; width + 2],
            current: vec![0; width + 2],
            column_k: vec![0; width],
            k: 0,
            run_state: 0,
        }
    }

    /// Decodes the next line into `out` (`width` values).
    pub fn decode_line(&mut self, out: &mut [i32]) -> Result<(), DecodeError> {
        if self.width == 0 {
            return Ok(());
        }
        std::mem::swap(&mut self.previous, &mut self.current);
        match (self.coding, self.line) {
            (Coding::Predicted, 0) => self.predicted_top()?,
            (Coding::Predicted, _) => self.predicted()?,
            (Coding::Unpredicted, 0) => self.unpredicted_top()?,
            (Coding::Unpredicted, _) => self.unpredicted()?,
        }
        self.line += 1;
        out.copy_from_slice(&self.current[1..=self.width]);
        Ok(())
    }

    fn code(&mut self) -> Result<u32, DecodeError> {
        self.bits.code(self.k)
    }

    fn predicted_top(&mut self) -> Result<(), DecodeError> {
        let cur = &mut self.current;
        cur[0] = 0;
        let mut at = 0;
        let mut remaining = self.width;
        while remaining > 1 {
            if cur[at] != 0 {
                cur[at + 1] = cur[at];
            } else {
                let run = self.bits.run(remaining, &mut self.run_state)?;
                if run > 0 {
                    cur[at + 1..=at + run].fill(0);
                    at += run;
                    remaining -= run;
                    if remaining == 0 {
                        break;
                    }
                }
                cur[at + 1] = 0;
            }
            let code = self.bits.code(self.k)?;
            cur[at + 1] = cur[at + 1].wrapping_add(signed(code));
            self.k = next_k(self.k, code, K_LIMIT);
            at += 1;
            remaining -= 1;
        }
        if remaining == 1 {
            let code = self.bits.code(self.k)?;
            cur[at + 1] = cur[at].wrapping_add(signed(code));
            self.k = next_k(self.k, code, K_LIMIT);
            at += 1;
        }
        cur[at + 1] = cur[at].wrapping_add(1);
        Ok(())
    }

    fn predicted(&mut self) -> Result<(), DecodeError> {
        self.current[0] = self.previous[1];
        let mut at = 0;
        let mut remaining = self.width;
        while remaining > 1 {
            let left = self.current[at];
            if left != self.previous[at + 1] || left != self.previous[at + 2] {
                self.predicted_symbol(at, true, true)?;
                at += 1;
            } else {
                let run = self.bits.run(remaining, &mut self.run_state)?;
                self.current[at + 1..=at + run].fill(left);
                at += run;
                remaining -= run;
                if remaining > 0 {
                    self.predicted_symbol(at, false, remaining > 1)?;
                    at += 1;
                }
            }
            remaining = remaining.saturating_sub(1);
            if remaining == 0 {
                break;
            }
        }
        if remaining == 1 {
            self.predicted_symbol(at, true, false)?;
            at += 1;
        }
        self.current[at + 1] = self.current[at].wrapping_add(1);
        Ok(())
    }

    /// One value predicted from its neighbours; mid-line, the step to the next
    /// column above also steers the Golomb parameter.
    fn predicted_symbol(
        &mut self,
        at: usize,
        use_median: bool,
        more: bool,
    ) -> Result<(), DecodeError> {
        let (prev, cur) = (&self.previous, &mut self.current);
        let prediction =
            if use_median { median(cur[at], prev[at + 1], prev[at]) } else { prev[at + 1] };
        let code = self.bits.code(self.k)?;
        cur[at + 1] = prediction.wrapping_add(signed(code));
        let context = if more {
            let step = prev[at + 2].wrapping_sub(prev[at + 1]).wrapping_mul(2).unsigned_abs();
            code.wrapping_add(step) >> 1
        } else {
            code
        };
        self.k = next_k(self.k, context, K_LIMIT);
        Ok(())
    }

    fn unpredicted_top(&mut self) -> Result<(), DecodeError> {
        self.current[0] = 0;
        let mut at = 0;
        let mut remaining = self.width;
        while remaining > 1 {
            if self.current[at] != 0 {
                let code = self.code()?;
                self.current[at + 1] = signed(code);
                self.k = next_k(self.k, code, 0);
            } else {
                let run = self.bits.run(remaining, &mut self.run_state)?;
                if run > 0 {
                    self.current[at + 1..=at + run].fill(0);
                    self.column_k[at..at + run].fill(0);
                    at += run;
                    remaining -= run;
                    if remaining == 0 {
                        break;
                    }
                }
                // A run ends at a non-zero value, so zero is not coded.
                let code = self.code()?;
                self.current[at + 1] = signed(code.wrapping_add(1));
                self.k = next_k(self.k, code, 0);
            }
            self.column_k[at] = self.k;
            at += 1;
            remaining -= 1;
        }
        if remaining == 1 {
            let code = self.code()?;
            self.current[at + 1] = signed(code);
            self.k = next_k(self.k, code, 0);
            self.column_k[at] = self.k;
            at += 1;
        }
        self.current[at + 1] = 0;
        Ok(())
    }

    fn unpredicted(&mut self) -> Result<(), DecodeError> {
        let width = self.width;
        self.current[0] = 0;
        let mut column = 0;
        while column + 1 < width {
            let busy = self.previous[column + 2] | self.previous[column + 1] | self.current[column];
            if busy != 0 {
                let code = self.code()?;
                self.current[column + 1] = signed(code);
                self.k = self.k_with_context(code, column);
            } else {
                let run = self.bits.run(width - column, &mut self.run_state)?;
                self.current[column + 1..=column + run].fill(0);
                self.column_k[column..column + run].fill(0);
                column += run;
                if column + 1 >= width {
                    if column + 1 == width {
                        let code = self.code()?;
                        self.current[column + 1] = signed(code.wrapping_add(1));
                        self.k = next_k(self.k, code, K_LIMIT);
                        self.column_k[column] = self.k;
                    }
                    column += 1;
                    continue;
                }
                let code = self.code()?;
                self.current[column + 1] = signed(code.wrapping_add(1));
                self.k = self.k_with_context(code, column);
            }
            self.column_k[column] = self.k;
            column += 1;
        }
        if column + 1 == width {
            let code = self.code()?;
            self.current[column + 1] = signed(code);
            self.k = next_k(self.k, code, K_LIMIT);
            self.column_k[column] = self.k;
        }
        Ok(())
    }

    /// The next parameter also leans towards the one the line above used.
    fn k_with_context(&self, code: u32, column: usize) -> u32 {
        let k = next_k(self.k, code, 0);
        let above = self.column_k[column + 1];
        if i64::from(above) - i64::from(k) <= 1 { k.min(K_LIMIT) } else { k + 1 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(data: &[u8], width: usize, coding: Coding) -> Vec<i32> {
        let mut out = vec![0; width];
        BandDecoder::new(data, width, coding).decode_line(&mut out).unwrap();
        out
    }

    #[test]
    fn decodes_a_predicted_top_line() {
        // No run (0), +2 (00001), then k = 1: 0 (1 0), then k = 0: -1 (01).
        assert_eq!(line(&[0b0000_0110, 0b0100_0000], 3, Coding::Predicted), [2, 2, 1]);
    }

    #[test]
    fn decodes_an_unpredicted_top_line_with_a_run() {
        // Run of one zero (1 0), run end value 3 coded as 5 (000001), then k = 1: -1 (1 1).
        assert_eq!(line(&[0b1000_0001, 0b1100_0000], 3, Coding::Unpredicted), [0, 3, -1]);
    }

    #[test]
    fn runs_cover_whole_lines() {
        let mut decoder = BandDecoder::new(&[0xFF; 8], 24, Coding::Predicted);
        let mut out = vec![1; 24];
        for _ in 0..3 {
            decoder.decode_line(&mut out).unwrap();
            assert!(out.iter().all(|&v| v == 0));
        }
    }
}
