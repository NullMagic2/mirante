//! Module: mirante_codec::tiff
//! Purpose: Parses TIFF headers and image file directories (IFDs).
//! Created: 2026-11-16
//! Architecture: Shared by the TIFF plugin and the RAW plugin, since DNG, CR2,
//! NEF, ARW and most other camera formats are TIFF containers. Values are read
//! on demand from the borrowed file bytes. BigTIFF is reported as unsupported.

use crate::{ByteReader, Endian};
use mirante_core::DecodeError;

const MAGIC: u16 = 42;
const BIGTIFF_MAGIC: u16 = 43;
/// Guards against IFD loops and absurd files.
const MAX_IFDS: usize = 256;
const MAX_ENTRIES: u16 = 4096;
const INLINE_VALUE_BYTES: usize = 4;

pub mod tag {
    pub const NEW_SUBFILE_TYPE: u16 = 254;
    pub const IMAGE_WIDTH: u16 = 256;
    pub const IMAGE_LENGTH: u16 = 257;
    pub const BITS_PER_SAMPLE: u16 = 258;
    pub const COMPRESSION: u16 = 259;
    pub const PHOTOMETRIC: u16 = 262;
    pub const MAKE: u16 = 271;
    pub const MODEL: u16 = 272;
    pub const STRIP_OFFSETS: u16 = 273;
    pub const ORIENTATION: u16 = 274;
    pub const SAMPLES_PER_PIXEL: u16 = 277;
    pub const ROWS_PER_STRIP: u16 = 278;
    pub const STRIP_BYTE_COUNTS: u16 = 279;
    pub const PLANAR_CONFIGURATION: u16 = 284;
    pub const PREDICTOR: u16 = 317;
    pub const COLOR_MAP: u16 = 320;
    pub const TILE_WIDTH: u16 = 322;
    pub const TILE_LENGTH: u16 = 323;
    pub const TILE_OFFSETS: u16 = 324;
    pub const TILE_BYTE_COUNTS: u16 = 325;
    pub const SUB_IFDS: u16 = 330;
    pub const EXTRA_SAMPLES: u16 = 338;
    pub const SAMPLE_FORMAT: u16 = 339;
    pub const JPEG_INTERCHANGE_FORMAT: u16 = 513;
    pub const JPEG_INTERCHANGE_FORMAT_LENGTH: u16 = 514;
    pub const ICC_PROFILE: u16 = 34675;
    pub const EXIF_IFD: u16 = 34665;
}

/// TIFF field types, numbered as in the specification.
mod field {
    pub const BYTE: u16 = 1;
    pub const ASCII: u16 = 2;
    pub const SHORT: u16 = 3;
    pub const LONG: u16 = 4;
    pub const RATIONAL: u16 = 5;
    pub const SBYTE: u16 = 6;
    pub const UNDEFINED: u16 = 7;
    pub const SSHORT: u16 = 8;
    pub const SLONG: u16 = 9;
    pub const SRATIONAL: u16 = 10;
    pub const FLOAT: u16 = 11;
    pub const DOUBLE: u16 = 12;
    pub const IFD: u16 = 13;

    pub fn size(field_type: u16) -> Option<usize> {
        match field_type {
            BYTE | ASCII | SBYTE | UNDEFINED => Some(1),
            SHORT | SSHORT => Some(2),
            LONG | SLONG | FLOAT | IFD => Some(4),
            RATIONAL | SRATIONAL | DOUBLE => Some(8),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Entry {
    pub tag: u16,
    pub field_type: u16,
    pub count: u32,
    /// Absolute file offset of the value bytes, inline or not.
    value_offset: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Ifd {
    pub offset: usize,
    pub entries: Vec<Entry>,
}

impl Ifd {
    pub fn get(&self, tag: u16) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.tag == tag)
    }
}

pub struct Tiff<'a> {
    reader: ByteReader<'a>,
    /// The main IFD chain, in file order.
    pub ifds: Vec<Ifd>,
}

impl<'a> Tiff<'a> {
    pub fn is_tiff(header: &[u8]) -> bool {
        header.starts_with(b"II*\0") || header.starts_with(b"MM\0*")
    }

    /// Parses a TIFF starting at byte 0 of `data`. Also accepts the RAW
    /// variants (ORF, RW2) that change the magic but keep the IFD layout.
    pub fn parse(data: &'a [u8]) -> Result<Self, DecodeError> {
        Self::parse_with_magic(data, &[MAGIC, 0x4F52, 0x5352, 0x0055])
    }

    pub fn parse_with_magic(data: &'a [u8], magics: &[u16]) -> Result<Self, DecodeError> {
        let mut reader = ByteReader::new(data, Endian::Little);
        let endian = match reader.bytes(2)? {
            b"II" => Endian::Little,
            b"MM" => Endian::Big,
            _ => return Err(DecodeError::corrupt("not a TIFF byte-order mark")),
        };
        reader.set_endian(endian);
        let magic = reader.u16()?;

        if magic == BIGTIFF_MAGIC {
            return Err(DecodeError::unsupported("BigTIFF"));
        }
        if !magics.contains(&magic) {
            return Err(DecodeError::corrupt("bad TIFF magic number"));
        }
        let first = reader.u32()? as usize;
        let mut tiff = Self { reader, ifds: Vec::new() };
        tiff.ifds = tiff.ifd_chain(first)?;
        Ok(tiff)
    }

    /// IFDs inside other structures (maker notes, decrypted blocks) that have no
    /// TIFF header: call `read_ifd` with offsets into `data`.
    pub fn headerless(data: &'a [u8], endian: Endian) -> Self {
        Self { reader: ByteReader::new(data, endian), ifds: Vec::new() }
    }

    pub fn data(&self) -> &'a [u8] {
        self.reader.data()
    }

    pub fn endian(&self) -> Endian {
        self.reader.endian()
    }

    /// Follows next-IFD links from `offset` until the chain ends.
    pub fn ifd_chain(&self, mut offset: usize) -> Result<Vec<Ifd>, DecodeError> {
        let mut ifds = Vec::new();

        while offset != 0 {
            if ifds.len() == MAX_IFDS || ifds.iter().any(|ifd: &Ifd| ifd.offset == offset) {
                return Err(DecodeError::corrupt("TIFF IFD chain loops or is too long"));
            }
            let (ifd, next) = self.read_ifd(offset)?;
            ifds.push(ifd);
            offset = next;
        }
        Ok(ifds)
    }

    /// Reads one IFD and returns it with the offset of the next one (0 = none).
    pub fn read_ifd(&self, offset: usize) -> Result<(Ifd, usize), DecodeError> {
        let mut reader = self.reader.at(offset)?;
        let count = reader.u16()?;

        if count > MAX_ENTRIES {
            return Err(DecodeError::corrupt("too many TIFF directory entries"));
        }
        let mut entries = Vec::with_capacity(usize::from(count));

        for _ in 0..count {
            let tag = reader.u16()?;
            let field_type = reader.u16()?;
            let value_count = reader.u32()?;
            let inline_at = reader.position();
            let pointer = reader.u32()? as usize;

            // Unknown field types are skipped, as the specification requires.
            let Some(size) = field::size(field_type) else {
                continue;
            };
            let length = size.checked_mul(value_count as usize);
            let value_offset = match length {
                Some(length) if length <= INLINE_VALUE_BYTES => inline_at,
                Some(_) => pointer,
                None => return Err(DecodeError::corrupt("TIFF value size overflows")),
            };
            entries.push(Entry { tag, field_type, count: value_count, value_offset });
        }
        let next = reader.u32()? as usize;
        Ok((Ifd { offset, entries }, next))
    }

    /// The SubIFDs (or EXIF IFD) an entry points to.
    pub fn child_ifds(&self, ifd: &Ifd, tag: u16) -> Result<Vec<Ifd>, DecodeError> {
        let Some(entry) = ifd.get(tag) else {
            return Ok(Vec::new());
        };
        let offsets = self.unsigned(entry)?;
        offsets.iter().take(MAX_IFDS).map(|&o| Ok(self.read_ifd(o as usize)?.0)).collect()
    }

    pub fn raw_bytes(&self, entry: &Entry) -> Result<&'a [u8], DecodeError> {
        let size = field::size(entry.field_type).expect("parsed entries have known types");
        self.reader.at(entry.value_offset)?.bytes(size * entry.count as usize)
    }

    fn values(&self, entry: &Entry) -> Result<ByteReader<'a>, DecodeError> {
        self.raw_bytes(entry)?;
        self.reader.at(entry.value_offset)
    }

    /// Integer values of a BYTE, SHORT, LONG or IFD entry.
    pub fn unsigned(&self, entry: &Entry) -> Result<Vec<u32>, DecodeError> {
        let mut reader = self.values(entry)?;
        (0..entry.count)
            .map(|_| match entry.field_type {
                field::BYTE | field::UNDEFINED => reader.u8().map(u32::from),
                field::SHORT => reader.u16().map(u32::from),
                field::LONG | field::IFD => reader.u32(),
                _ => Err(DecodeError::corrupt(format!("tag {} is not an integer", entry.tag))),
            })
            .collect()
    }

    /// Any numeric entry as floating point; rationals are divided out.
    pub fn numbers(&self, entry: &Entry) -> Result<Vec<f64>, DecodeError> {
        let mut reader = self.values(entry)?;
        (0..entry.count)
            .map(|_| {
                Ok(match entry.field_type {
                    field::BYTE | field::UNDEFINED => f64::from(reader.u8()?),
                    field::SBYTE => f64::from(reader.u8()? as i8),
                    field::SHORT => f64::from(reader.u16()?),
                    field::SSHORT => f64::from(reader.u16()? as i16),
                    field::LONG | field::IFD => f64::from(reader.u32()?),
                    field::SLONG => f64::from(reader.i32()?),
                    field::RATIONAL => ratio(f64::from(reader.u32()?), f64::from(reader.u32()?)),
                    field::SRATIONAL => ratio(f64::from(reader.i32()?), f64::from(reader.i32()?)),
                    field::FLOAT => f64::from(reader.f32()?),
                    field::DOUBLE => reader.f64()?,
                    _ => return Err(DecodeError::corrupt("text where a number was expected")),
                })
            })
            .collect()
    }

    pub fn ascii(&self, entry: &Entry) -> Result<String, DecodeError> {
        let bytes = self.raw_bytes(entry)?;
        let text = bytes.split(|&b| b == 0).next().unwrap_or_default();
        Ok(String::from_utf8_lossy(text).trim().to_owned())
    }

    /// First integer value of a tag, the common case for scalar fields.
    pub fn get_u32(&self, ifd: &Ifd, tag: u16) -> Result<Option<u32>, DecodeError> {
        match ifd.get(tag) {
            Some(entry) => Ok(self.unsigned(entry)?.first().copied()),
            None => Ok(None),
        }
    }
}

fn ratio(numerator: f64, denominator: f64) -> f64 {
    if denominator == 0.0 { 0.0 } else { numerator / denominator }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Little-endian TIFF with one IFD: width 7 (SHORT, inline), make "Ab" (inline).
    fn sample() -> Vec<u8> {
        let mut file = b"II*\0".to_vec();
        file.extend(8_u32.to_le_bytes());
        file.extend(2_u16.to_le_bytes());
        file.extend([0x00, 0x01, 3, 0, 1, 0, 0, 0, 7, 0, 0, 0]);
        file.extend([0x0F, 0x01, 2, 0, 3, 0, 0, 0, b'A', b'b', 0, 0]);
        file.extend(0_u32.to_le_bytes());
        file
    }

    #[test]
    fn reads_inline_values() {
        let data = sample();
        let tiff = Tiff::parse(&data).unwrap();
        let ifd = &tiff.ifds[0];
        assert_eq!(tiff.get_u32(ifd, tag::IMAGE_WIDTH).unwrap(), Some(7));
        assert_eq!(tiff.ascii(ifd.get(tag::MAKE).unwrap()).unwrap(), "Ab");
    }

    #[test]
    fn reads_headerless_ifds() {
        let data = sample();
        let tiff = Tiff::headerless(&data[8..], Endian::Little);
        let (ifd, next) = tiff.read_ifd(0).unwrap();
        assert_eq!((tiff.get_u32(&ifd, tag::IMAGE_WIDTH).unwrap(), next), (Some(7), 0));
    }

    #[test]
    fn rejects_ifd_loops() {
        let mut data = sample();
        let next = data.len() - 4;
        data[next..].copy_from_slice(&8_u32.to_le_bytes());
        assert!(Tiff::parse(&data).is_err());
    }
}
