//! Module: mirante_codec::reader
//! Purpose: Bounds-checked sequential reads of integers and byte slices.
//! Created: 2026-11-16
//! Architecture: Every read past the end becomes a DecodeError, so parsers can
//! use `?` freely on untrusted files without indexing panics.

use mirante_core::DecodeError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endian {
    Little,
    Big,
}

#[derive(Clone)]
pub struct ByteReader<'a> {
    data: &'a [u8],
    position: usize,
    endian: Endian,
}

fn truncated() -> DecodeError {
    DecodeError::corrupt("unexpected end of data")
}

impl<'a> ByteReader<'a> {
    pub fn new(data: &'a [u8], endian: Endian) -> Self {
        Self { data, position: 0, endian }
    }

    /// A reader over the same data, positioned at `offset`.
    pub fn at(&self, offset: usize) -> Result<Self, DecodeError> {
        if offset > self.data.len() {
            return Err(truncated());
        }
        Ok(Self { position: offset, ..self.clone() })
    }

    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    pub fn position(&self) -> usize {
        self.position
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.position
    }

    pub fn endian(&self) -> Endian {
        self.endian
    }

    pub fn set_endian(&mut self, endian: Endian) {
        self.endian = endian;
    }

    pub fn bytes(&mut self, length: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.position.checked_add(length).ok_or_else(truncated)?;
        let slice = self.data.get(self.position..end).ok_or_else(truncated)?;
        self.position = end;
        Ok(slice)
    }

    pub fn skip(&mut self, length: usize) -> Result<(), DecodeError> {
        self.bytes(length).map(drop)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        Ok(self.bytes(N)?.try_into().expect("slice has exactly N bytes"))
    }

    pub fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.array::<1>()?[0])
    }

    pub fn u16(&mut self) -> Result<u16, DecodeError> {
        let bytes = self.array()?;
        Ok(match self.endian {
            Endian::Little => u16::from_le_bytes(bytes),
            Endian::Big => u16::from_be_bytes(bytes),
        })
    }

    pub fn u32(&mut self) -> Result<u32, DecodeError> {
        let bytes = self.array()?;
        Ok(match self.endian {
            Endian::Little => u32::from_le_bytes(bytes),
            Endian::Big => u32::from_be_bytes(bytes),
        })
    }

    pub fn u64(&mut self) -> Result<u64, DecodeError> {
        let bytes = self.array()?;
        Ok(match self.endian {
            Endian::Little => u64::from_le_bytes(bytes),
            Endian::Big => u64::from_be_bytes(bytes),
        })
    }

    pub fn i32(&mut self) -> Result<i32, DecodeError> {
        Ok(self.u32()? as i32)
    }

    pub fn f32(&mut self) -> Result<f32, DecodeError> {
        Ok(f32::from_bits(self.u32()?))
    }

    pub fn f64(&mut self) -> Result<f64, DecodeError> {
        Ok(f64::from_bits(self.u64()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_both_endians() {
        let data = [0x12, 0x34, 0x56, 0x78];
        assert_eq!(ByteReader::new(&data, Endian::Big).u32().unwrap(), 0x1234_5678);
        assert_eq!(ByteReader::new(&data, Endian::Little).u16().unwrap(), 0x3412);
    }

    #[test]
    fn reading_past_the_end_is_an_error() {
        let mut reader = ByteReader::new(&[1, 2], Endian::Big);
        assert!(reader.u32().is_err());
        assert_eq!(reader.position(), 0);
    }
}
