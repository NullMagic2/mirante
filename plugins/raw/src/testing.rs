//! Module: mirante_format_raw::testing
//! Purpose: Builds small little-endian TIFF/DNG files and ISO boxes inside tests.
//! Created: 2026-11-16
//! Architecture: Test-only. Out-of-line values, data blobs and SubIFDs are appended
//! after each IFD and their offsets patched in, so callers never compute offsets.

pub enum Value {
    Short(Vec<u16>),
    Long(Vec<u32>),
    /// Signed rationals with a fixed denominator, enough for matrices and levels.
    SRational(Vec<f64>),
    Undefined(Vec<u8>),
    /// A LONG pointing at these bytes, appended to the file.
    DataOffset(Vec<u8>),
    /// A LONG list pointing at further IFDs.
    SubIfds(Vec<Vec<(u16, Value)>>),
}

const RATIONAL_DENOMINATOR: i32 = 10_000;

/// A TIFF with a single IFD0.
pub fn tiff(ifd: Vec<(u16, Value)>) -> Vec<u8> {
    let mut out = b"II*\0".to_vec();
    out.extend(8_u32.to_le_bytes());
    write_ifd(&mut out, ifd);
    out
}

fn write_ifd(out: &mut Vec<u8>, fields: Vec<(u16, Value)>) -> u32 {
    let start = out.len();
    out.extend((fields.len() as u16).to_le_bytes());
    out.resize(start + 2 + fields.len() * 12 + 4, 0);

    for (index, (tag, value)) in fields.into_iter().enumerate() {
        let (field_type, count, bytes) = match value {
            Value::Short(v) => (3_u16, v.len(), v.iter().flat_map(|x| x.to_le_bytes()).collect()),
            Value::Long(v) => (4, v.len(), v.iter().flat_map(|x| x.to_le_bytes()).collect()),
            Value::SRational(v) => {
                let pairs = v.iter().map(|x| {
                    ((x * f64::from(RATIONAL_DENOMINATOR)).round() as i32, RATIONAL_DENOMINATOR)
                });
                (
                    10,
                    v.len(),
                    pairs.flat_map(|(n, d)| [n.to_le_bytes(), d.to_le_bytes()].concat()).collect(),
                )
            }
            Value::Undefined(v) => (7, v.len(), v),
            Value::DataOffset(data) => {
                let offset = out.len() as u32;
                out.extend(data);
                (4, 1, offset.to_le_bytes().to_vec())
            }
            Value::SubIfds(ifds) => {
                let offsets: Vec<u32> = ifds.into_iter().map(|ifd| write_ifd(out, ifd)).collect();
                (4, offsets.len(), offsets.iter().flat_map(|x| x.to_le_bytes()).collect())
            }
        };
        let entry = start + 2 + index * 12;
        let mut header = tag.to_le_bytes().to_vec();
        header.extend(field_type.to_le_bytes());
        header.extend((count as u32).to_le_bytes());
        let bytes: Vec<u8> = bytes;
        let value = if bytes.len() <= 4 {
            let mut inline = bytes;
            inline.resize(4, 0);
            inline
        } else {
            let offset = out.len() as u32;
            out.extend(bytes);
            offset.to_le_bytes().to_vec()
        };
        header.extend(value);
        out[entry..entry + 12].copy_from_slice(&header);
    }
    start as u32
}

/// One ISO-BMFF box with a 32-bit size.
pub fn iso_box(box_type: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend(box_type);
    out.extend(payload);
    out
}
