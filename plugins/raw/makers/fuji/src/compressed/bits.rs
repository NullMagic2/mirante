//! Module: mirante_raw_fuji::compressed::bits
//! Purpose: MSB-first bit reading with zero-run counting for Golomb-Rice codes.
//! Created: 2026-11-16
//! Architecture: Wraps mirante_raw::bits::BitReader. Past the end it reads
//! zeros, but only one byte's worth may be consumed (as LibRaw's fuji decoder
//! allows); anything more is a truncated block.

use mirante_core::DecodeError;
use mirante_raw::bits;

/// Bytes of zeros a block may consume past its end.
const END_SLACK: u64 = 1;
const WINDOW: u32 = 32;

/// The shared MSB-first reader, with Fujifilm's bounded overrun and zero runs.
pub struct BitReader<'a> {
    inner: bits::BitReader<'a>,
    limit: u64,
}

fn truncated() -> DecodeError {
    DecodeError::corrupt("Fujifilm compressed block is truncated")
}

impl<'a> BitReader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        let limit = (bytes.len() as u64 + END_SLACK) * 8;
        Self { inner: bits::BitReader::new(bytes), limit }
    }

    fn check_end(&self) -> Result<(), DecodeError> {
        if self.inner.consumed_bits() > self.limit {
            return Err(truncated());
        }
        Ok(())
    }

    /// Counts zero bits up to the next one bit, consuming both.
    pub fn zeros(&mut self) -> Result<u32, DecodeError> {
        let mut count = 0;
        loop {
            let window = self.inner.peek(WINDOW);
            if window != 0 {
                let run = window.leading_zeros();
                self.inner.consume(run + 1);
                self.check_end()?;
                return Ok(count + run);
            }
            count += WINDOW;
            self.inner.consume(WINDOW);
            self.check_end()?;
        }
    }

    /// Reads `count` (at most 32) bits as an unsigned number.
    pub fn read(&mut self, count: u32) -> Result<u32, DecodeError> {
        let value = self.inner.read(count);
        self.check_end()?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_runs_and_fields() {
        let mut bits = BitReader::new(&[0b0010_1101, 0b1000_0000, 0x00, 0x01]);
        assert_eq!(bits.zeros().unwrap(), 2);
        assert_eq!(bits.read(3).unwrap(), 0b011);
        assert_eq!(bits.read(0).unwrap(), 0);
        assert_eq!(bits.zeros().unwrap(), 1);
        assert_eq!(bits.zeros().unwrap(), 0);
        assert_eq!(bits.zeros().unwrap(), 7 + 8 + 7);
    }

    #[test]
    fn allows_one_byte_past_the_end_then_fails() {
        let mut bits = BitReader::new(&[0xFF]);
        assert_eq!(bits.read(16).unwrap(), 0xFF00);
        assert!(bits.read(1).is_err());
        assert!(BitReader::new(&[0, 0, 0]).zeros().is_err());
    }
}
