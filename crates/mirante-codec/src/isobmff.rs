//! Module: mirante_codec::isobmff
//! Purpose: Reads ISO base media file format boxes (HEIF, AVIF, CR3).
//! Created: 2026-11-16
//! Architecture: Boxes borrow the file bytes and remember their absolute payload
//! offsets, because item locations and sample tables point into the file. Format
//! structures (HEIF items, CR3 tracks) live in the plugins that own them.

use crate::{ByteReader, Endian};
use mirante_core::DecodeError;

pub type FourCc = [u8; 4];

const SIZE_FIELD_BYTES: usize = 4;
const TYPE_FIELD_BYTES: usize = 4;
const USER_TYPE_BYTES: usize = 16;
const SIZE_TO_END: u64 = 0;
const SIZE_IS_LARGE: u64 = 1;
const UUID: FourCc = *b"uuid";
const FILE_TYPE: FourCc = *b"ftyp";

/// Brands that promise a HEVC-coded (or generic HEIF) still image.
const HEIF_BRANDS: [FourCc; 6] = [*b"heic", *b"heix", *b"hevc", *b"hevx", *b"heim", *b"heis"];
/// Generic HEIF brands, which AVIF files list too.
const GENERIC_HEIF_BRANDS: [FourCc; 2] = [*b"mif1", *b"msf1"];
const AVIF_BRANDS: [FourCc; 2] = [*b"avif", *b"avis"];
/// Canon CR3 is ISO-BMFF too, but it is a RAW file, not HEIF.
const CANON_CR3_BRAND: FourCc = *b"crx ";

/// One box: its type, optional uuid user type, and the payload after the header.
#[derive(Clone, Copy, Debug)]
pub struct IsoBox<'a> {
    pub box_type: FourCc,
    pub user_type: Option<[u8; USER_TYPE_BYTES]>,
    /// Absolute offset of `payload` within the bytes the walk started from.
    pub payload_offset: usize,
    pub payload: &'a [u8],
}

/// A FullBox payload with its version and 24-bit flags split off.
#[derive(Clone, Copy, Debug)]
pub struct FullBox<'a> {
    pub version: u8,
    pub flags: u32,
    pub payload_offset: usize,
    pub payload: &'a [u8],
}

impl<'a> IsoBox<'a> {
    /// A big-endian reader over the payload.
    pub fn reader(&self) -> ByteReader<'a> {
        ByteReader::new(self.payload, Endian::Big)
    }

    /// The boxes nested directly inside this one.
    pub fn children(&self) -> Boxes<'a> {
        Boxes::new(self.payload, self.payload_offset)
    }

    pub fn full(&self) -> Result<FullBox<'a>, DecodeError> {
        let mut reader = self.reader();
        let word = reader.u32()?;
        Ok(FullBox {
            version: (word >> 24) as u8,
            flags: word & 0x00FF_FFFF,
            payload_offset: self.payload_offset + reader.position(),
            payload: &self.payload[reader.position()..],
        })
    }
}

impl<'a> FullBox<'a> {
    pub fn reader(&self) -> ByteReader<'a> {
        ByteReader::new(self.payload, Endian::Big)
    }

    /// Child boxes that start `skip` bytes into the payload (after counts etc.).
    pub fn children_after(&self, skip: usize) -> Result<Boxes<'a>, DecodeError> {
        let rest = self.payload.get(skip..).ok_or_else(truncated)?;
        Ok(Boxes::new(rest, self.payload_offset + skip))
    }
}

/// Iterates over consecutive boxes; stops after the first malformed one.
#[derive(Clone, Debug)]
pub struct Boxes<'a> {
    data: &'a [u8],
    base: usize,
    position: usize,
    failed: bool,
}

/// The top-level boxes of a file.
pub fn boxes(data: &[u8]) -> Boxes<'_> {
    Boxes::new(data, 0)
}

fn truncated() -> DecodeError {
    DecodeError::corrupt("ISO-BMFF box runs past its parent")
}

impl<'a> Boxes<'a> {
    fn new(data: &'a [u8], base: usize) -> Self {
        Self { data, base, position: 0, failed: false }
    }

    /// The first box of the given type, if any.
    pub fn find(self, box_type: FourCc) -> Result<Option<IsoBox<'a>>, DecodeError> {
        for item in self {
            let item = item?;
            if item.box_type == box_type {
                return Ok(Some(item));
            }
        }
        Ok(None)
    }

    /// Like `find`, but a missing box is a corrupt file.
    pub fn require(self, box_type: FourCc) -> Result<IsoBox<'a>, DecodeError> {
        self.find(box_type)?
            .ok_or_else(|| DecodeError::corrupt(format!("missing '{}' box", fourcc_text(box_type))))
    }

    fn read_box(&mut self) -> Result<IsoBox<'a>, DecodeError> {
        let start = self.position;
        let mut reader = ByteReader::new(self.data, Endian::Big).at(start)?;
        let size = u64::from(reader.u32()?);
        let box_type: FourCc = reader.bytes(TYPE_FIELD_BYTES)?.try_into().expect("four bytes");
        let size = match size {
            SIZE_IS_LARGE => reader.u64()?,
            SIZE_TO_END => (self.data.len() - start) as u64,
            size => size,
        };
        let user_type = match box_type {
            UUID => Some(reader.bytes(USER_TYPE_BYTES)?.try_into().expect("sixteen bytes")),
            _ => None,
        };
        let header = reader.position() - start;
        let end = usize::try_from(size)
            .ok()
            .and_then(|size| start.checked_add(size))
            .filter(|&end| end <= self.data.len() && end >= reader.position())
            .ok_or_else(truncated)?;
        self.position = end;
        Ok(IsoBox {
            box_type,
            user_type,
            payload_offset: self.base + start + header,
            payload: &self.data[start + header..end],
        })
    }
}

impl<'a> Iterator for Boxes<'a> {
    type Item = Result<IsoBox<'a>, DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.position >= self.data.len() {
            return None;
        }
        let result = self.read_box();
        self.failed = result.is_err();
        Some(result)
    }
}

/// Printable form of a box or brand type for error messages.
pub fn fourcc_text(code: FourCc) -> String {
    code.iter().map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '?' }).collect()
}

/// Reads a NUL-terminated UTF-8 string, as used by `infe`, `hdlr` and `auxC`.
pub fn c_string(reader: &mut ByteReader<'_>) -> Result<String, DecodeError> {
    let rest = &reader.data()[reader.position()..];
    let length = rest.iter().position(|&b| b == 0);
    let text = String::from_utf8_lossy(&rest[..length.unwrap_or(rest.len())]).into_owned();
    reader.skip(length.map_or(rest.len(), |length| length + 1))?;
    Ok(text)
}

/// The `ftyp` box: which specifications the file claims to follow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileType {
    pub major_brand: FourCc,
    pub minor_version: u32,
    pub compatible_brands: Vec<FourCc>,
}

impl FileType {
    /// Reads the leading `ftyp` box; tolerates a header cut short while sniffing.
    pub fn read(data: &[u8]) -> Option<Self> {
        let size = u32::from_be_bytes(data.get(..SIZE_FIELD_BYTES)?.try_into().ok()?) as usize;
        if data.get(SIZE_FIELD_BYTES..SIZE_FIELD_BYTES + TYPE_FIELD_BYTES)? != FILE_TYPE {
            return None;
        }
        let header = SIZE_FIELD_BYTES + TYPE_FIELD_BYTES;
        let end = size.min(data.len());
        let body = data.get(header..end.max(header))?;
        let major_brand = body.get(..4)?.try_into().ok()?;
        let minor_version = u32::from_be_bytes(body.get(4..8)?.try_into().ok()?);
        let compatible_brands =
            body[8..].chunks_exact(4).map(|c| c.try_into().expect("four bytes")).collect();
        Some(Self { major_brand, minor_version, compatible_brands })
    }

    pub fn has_brand(&self, brand: FourCc) -> bool {
        self.major_brand == brand || self.compatible_brands.contains(&brand)
    }

    fn has_any(&self, brands: &[FourCc]) -> bool {
        brands.iter().any(|&brand| self.has_brand(brand))
    }
}

/// True for a HEIF still image: a HEVC brand, or a generic HEIF brand in a file
/// that is neither AVIF nor a Canon CR3.
pub fn is_heif(header: &[u8]) -> bool {
    let Some(file_type) = FileType::read(header) else {
        return false;
    };
    if file_type.major_brand == CANON_CR3_BRAND {
        return false;
    }
    if file_type.has_any(&HEIF_BRANDS) {
        return true;
    }
    file_type.has_any(&GENERIC_HEIF_BRANDS) && !file_type.has_any(&AVIF_BRANDS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_box(box_type: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend(box_type);
        out.extend(payload);
        out
    }

    fn ftyp(major: &[u8; 4], compatible: &[&[u8; 4]]) -> Vec<u8> {
        let mut payload = major.to_vec();
        payload.extend(0_u32.to_be_bytes());
        compatible.iter().for_each(|brand| payload.extend(*brand));
        make_box(b"ftyp", &payload)
    }

    #[test]
    fn recognises_heif_brands() {
        assert!(is_heif(&ftyp(b"heic", &[b"mif1", b"heic"])));
        assert!(is_heif(&ftyp(b"mif1", &[b"mif1", b"heic"])));
        assert!(is_heif(&ftyp(b"mif1", &[b"mif1", b"miaf"])));
        assert!(!is_heif(&ftyp(b"avif", &[b"mif1", b"avif", b"miaf"])));
        assert!(!is_heif(&ftyp(b"crx ", &[b"crx ", b"isom", b"heic"])));
        assert!(!is_heif(&ftyp(b"isom", &[b"mp41"])));
        assert!(!is_heif(b"\0\0\0\x18ftyp"));
    }

    #[test]
    fn walks_nested_boxes_with_absolute_offsets() {
        let inner = make_box(b"abcd", &[1, 2, 3]);
        let mut file = make_box(b"free", &[0; 4]);
        file.extend(make_box(b"moov", &inner));
        let moov = boxes(&file).require(*b"moov").unwrap();
        let child = moov.children().require(*b"abcd").unwrap();
        assert_eq!(child.payload, &[1, 2, 3]);
        assert_eq!(&file[child.payload_offset..child.payload_offset + 3], &[1, 2, 3]);
    }

    #[test]
    fn reads_large_size_uuid_and_size_to_end() {
        let mut file = 1_u32.to_be_bytes().to_vec();
        file.extend(b"uuid");
        file.extend(34_u64.to_be_bytes());
        file.extend([7; 16]);
        file.extend([9, 9]);
        file.extend(0_u32.to_be_bytes());
        file.extend(b"mdat");
        file.extend([5; 6]);
        let all: Vec<_> = boxes(&file).collect::<Result<_, _>>().unwrap();
        assert_eq!(all[0].user_type, Some([7; 16]));
        assert_eq!(all[0].payload, &[9, 9]);
        assert_eq!(all[1].box_type, *b"mdat");
        assert_eq!(all[1].payload, &[5; 6]);
    }

    #[test]
    fn rejects_boxes_that_overrun_their_parent() {
        let mut file = make_box(b"meta", &[0; 4]);
        file[3] = 200;
        assert!(boxes(&file).next().unwrap().is_err());
        assert!(boxes(&[0, 0, 0, 3, b'a', b'b', b'c', b'd']).next().unwrap().is_err());
    }

    #[test]
    fn splits_full_box_header() {
        let file = make_box(b"pitm", &[1, 0, 0, 3, 0, 9]);
        let full = boxes(&file).require(*b"pitm").unwrap().full().unwrap();
        assert_eq!((full.version, full.flags, full.payload), (1, 3, &[0, 9][..]));
        assert_eq!(full.payload_offset, 12);
    }

    #[test]
    fn reads_c_strings() {
        let mut reader = ByteReader::new(b"hvc\0rest", Endian::Big);
        assert_eq!(c_string(&mut reader).unwrap(), "hvc");
        assert_eq!(reader.position(), 4);
        let mut unterminated = ByteReader::new(b"abc", Endian::Big);
        assert_eq!(c_string(&mut unterminated).unwrap(), "abc");
    }
}
