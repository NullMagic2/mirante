//! Module: mirante_format_heic::hevc::bitstream
//! Purpose: NAL unit framing, emulation-prevention removal and RBSP bit reading.
//! Created: 2026-11-16
//! Architecture: Shared by params.rs and slice.rs (Exp-Golomb headers); the
//! CABAC decoder reads the same RBSP bytes after the slice header.

use mirante_core::DecodeError;

/// NAL unit types the decoder acts on (H.265 Table 7-1).
pub mod nal {
    pub const VPS: u8 = 32;
    pub const SPS: u8 = 33;
    pub const PPS: u8 = 34;
}

/// A NAL unit with its two-byte header split out and emulation prevention removed.
pub struct Nal {
    pub nal_type: u8,
    pub layer_id: u8,
    pub rbsp: Vec<u8>,
}

impl Nal {
    pub fn parse(bytes: &[u8]) -> Result<Self, DecodeError> {
        let [first, second, payload @ ..] = bytes else {
            return Err(DecodeError::corrupt("HEVC NAL unit is too short"));
        };
        if first & 0x80 != 0 {
            return Err(DecodeError::corrupt("HEVC NAL forbidden bit is set"));
        }
        let layer_id = ((first & 1) << 5) | (second >> 3);
        Ok(Self { nal_type: (first >> 1) & 0x3F, layer_id, rbsp: unescape(payload) })
    }
}

/// Splits item data into NAL units, each preceded by a big-endian length.
pub fn split_length_prefixed(data: &[u8], length_size: u8) -> Result<Vec<&[u8]>, DecodeError> {
    let size = usize::from(length_size);
    if !matches!(size, 1 | 2 | 4) {
        return Err(DecodeError::corrupt("HEVC NAL length size"));
    }
    let mut units = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        if rest.len() < size {
            return Err(DecodeError::corrupt("truncated HEVC NAL length"));
        }
        let length = rest[..size].iter().fold(0_usize, |acc, &b| (acc << 8) | usize::from(b));
        rest = &rest[size..];
        if length > rest.len() {
            return Err(DecodeError::corrupt("HEVC NAL unit runs past the item"));
        }
        units.push(&rest[..length]);
        rest = &rest[length..];
    }
    Ok(units)
}

/// Removes emulation prevention bytes: 00 00 03 becomes 00 00.
fn unescape(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len());
    let mut zeros = 0;
    for &byte in payload {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        out.push(byte);
    }
    out
}

/// MSB-first reader over RBSP bytes for the Exp-Golomb coded headers.
pub struct BitReader<'a> {
    data: &'a [u8],
    position: usize,
}

/// Exp-Golomb codes longer than this cannot encode a valid 32-bit value.
const MAX_GOLOMB_ZEROS: u32 = 31;

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    pub fn bit(&mut self) -> Result<bool, DecodeError> {
        let byte = self.data.get(self.position / 8).ok_or_else(truncated)?;
        let bit = (byte >> (7 - self.position % 8)) & 1;
        self.position += 1;
        Ok(bit == 1)
    }

    pub fn flag(&mut self) -> Result<bool, DecodeError> {
        self.bit()
    }

    /// u(n) for n up to 32.
    pub fn bits(&mut self, count: u32) -> Result<u32, DecodeError> {
        let mut value = 0_u32;
        for _ in 0..count {
            value = (value << 1) | u32::from(self.bit()?);
        }
        Ok(value)
    }

    pub fn skip(&mut self, count: usize) -> Result<(), DecodeError> {
        self.position += count;
        if self.position > self.data.len() * 8 {
            return Err(truncated());
        }
        Ok(())
    }

    /// ue(v)
    pub fn ue(&mut self) -> Result<u32, DecodeError> {
        let mut zeros = 0;
        while !self.bit()? {
            zeros += 1;
            if zeros > MAX_GOLOMB_ZEROS {
                return Err(DecodeError::corrupt("HEVC Exp-Golomb code is too long"));
            }
        }
        let value = (1_u64 << zeros) - 1 + u64::from(self.bits(zeros)?);
        u32::try_from(value).map_err(|_| DecodeError::corrupt("HEVC Exp-Golomb overflow"))
    }

    /// se(v)
    pub fn se(&mut self) -> Result<i32, DecodeError> {
        let code = i64::from(self.ue()?);
        let value = if code % 2 == 1 { (code + 1) / 2 } else { -(code / 2) };
        i32::try_from(value).map_err(|_| DecodeError::corrupt("HEVC Exp-Golomb overflow"))
    }

    /// ue(v) that must not exceed `max`.
    pub fn ue_max(&mut self, max: u32, what: &str) -> Result<u32, DecodeError> {
        let value = self.ue()?;
        if value > max {
            return Err(DecodeError::corrupt(format!("HEVC {what} is out of range")));
        }
        Ok(value)
    }

    /// se(v) that must lie in `min..=max`.
    pub fn se_range(&mut self, min: i32, max: i32, what: &str) -> Result<i32, DecodeError> {
        let value = self.se()?;
        if !(min..=max).contains(&value) {
            return Err(DecodeError::corrupt(format!("HEVC {what} is out of range")));
        }
        Ok(value)
    }

    /// Skips to the next byte boundary and returns the byte offset reached.
    pub fn byte_align(&mut self) -> usize {
        self.position = self.position.div_ceil(8) * 8;
        self.position / 8
    }
}

fn truncated() -> DecodeError {
    DecodeError::corrupt("HEVC header is truncated")
}

/// Test helpers that write what `BitReader` reads.
#[cfg(test)]
pub mod writer {
    /// ue(v) as a (code, bit count) field for `pack`.
    pub fn ue(value: u32) -> (u32, u32) {
        let code = value + 1;
        (code, 2 * (32 - code.leading_zeros()) - 1)
    }

    pub fn se(value: i32) -> (u32, u32) {
        ue(if value > 0 { 2 * value as u32 - 1 } else { 2 * value.unsigned_abs() })
    }

    /// Packs (value, bit count) fields MSB first, then the RBSP stop bit and alignment.
    pub fn pack(fields: &[(u32, u32)]) -> Vec<u8> {
        let mut bits: Vec<bool> = Vec::new();
        for &(value, count) in fields {
            bits.extend((0..count).rev().map(|i| (value >> i) & 1 == 1));
        }
        bits.push(true);
        bits.chunks(8)
            .map(|c| c.iter().enumerate().fold(0, |b, (i, &v)| b | (u8::from(v) << (7 - i))))
            .collect()
    }

    /// Inserts emulation prevention bytes, the inverse of `unescape`.
    pub fn escape(rbsp: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(rbsp.len());
        let mut zeros = 0;
        for &byte in rbsp {
            if zeros >= 2 && byte <= 3 {
                out.push(3);
                zeros = 0;
            }
            zeros = if byte == 0 { zeros + 1 } else { 0 };
            out.push(byte);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_exp_golomb_codes() {
        // 1 | 010 | 011 | 00100 | 00101 -> ue 0, 1, 2; se +2, -2; then the stop bit.
        let mut reader = BitReader::new(&[0b1010_0110, 0b0100_0010, 0b1100_0000]);
        assert_eq!(reader.ue().unwrap(), 0);
        assert_eq!(reader.ue().unwrap(), 1);
        assert_eq!(reader.ue().unwrap(), 2);
        assert_eq!(reader.se().unwrap(), 2);
        assert_eq!(reader.se().unwrap(), -2);
        assert!(reader.bit().unwrap());
    }

    #[test]
    fn removes_emulation_prevention_and_splits_units() {
        let nal = Nal::parse(&[0x40, 0x01, 0, 0, 3, 1, 0, 0, 3]).unwrap();
        assert_eq!((nal.nal_type, nal.layer_id), (nal::VPS, 0));
        assert_eq!(nal.rbsp, [0, 0, 1, 0, 0]);
        assert_eq!(writer::escape(&nal.rbsp), [0, 0, 3, 1, 0, 0]);
        let units = split_length_prefixed(&[0, 2, 7, 8, 0, 1, 9], 2).unwrap();
        assert_eq!(units, [&[7, 8][..], &[9][..]]);
        assert!(split_length_prefixed(&[0, 5, 1], 2).is_err());
    }
}
