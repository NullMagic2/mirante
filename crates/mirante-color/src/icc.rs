//! Module: mirante_color::icc
//! Purpose: Reads matrix/TRC RGB and grey ICC profiles (v2 and v4).
//! Created: 2026-11-16
//! Architecture: Covers sRGB, Adobe RGB, Display P3, ProPhoto, camera and most
//! monitor profiles. LUT-only and CMYK profiles are reported as unsupported and
//! the caller falls back to sRGB.

use crate::{
    curve::Curve,
    matrix::{D50, Mat3, xy_to_xyz},
};
use mirante_codec::{ByteReader, Endian};
use mirante_core::DecodeError;

const HEADER_LENGTH: usize = 128;
const MAX_TAGS: u32 = 1024;
const SIGNATURE: &[u8; 4] = b"acsp";
const S15_FIXED16: f64 = 65536.0;
const U8_FIXED8: f32 = 256.0;

/// What a profile does: linear RGB to the D50 connection space, after curves.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub to_pcs: Mat3,
    pub curves: [Curve; 3],
}

struct Tag {
    signature: [u8; 4],
    offset: usize,
    length: usize,
}

pub fn parse(bytes: &[u8]) -> Result<Profile, DecodeError> {
    let mut header = ByteReader::new(bytes, Endian::Big);
    header.skip(16)?;
    let color_space = header.bytes(4)?;
    let pcs = header.bytes(4)?;
    header.skip(12)?;

    if header.bytes(4)? != SIGNATURE {
        return Err(DecodeError::corrupt("missing ICC signature"));
    }
    if pcs != b"XYZ " {
        return Err(DecodeError::unsupported("ICC profile with a Lab connection space"));
    }
    let tags = tag_table(bytes)?;
    let find = |signature: &[u8; 4]| tags.iter().find(|tag| &tag.signature == signature);
    let read_curve = |signature| {
        let tag = find(signature).ok_or_else(|| DecodeError::unsupported("ICC curve missing"))?;
        curve(tag_data(bytes, tag)?)
    };

    match color_space {
        b"RGB " => {
            let columns = [b"rXYZ", b"gXYZ", b"bXYZ"].map(|signature| {
                let tag =
                    find(signature).ok_or_else(|| DecodeError::unsupported("LUT-only ICC"))?;
                xyz(tag_data(bytes, tag)?)
            });
            let [r, g, b] = columns;
            let (r, g, b) = (r?, g?, b?);
            let to_pcs = [0, 1, 2].map(|row| [r[row], g[row], b[row]]);
            Ok(Profile {
                to_pcs,
                curves: [read_curve(b"rTRC")?, read_curve(b"gTRC")?, read_curve(b"bTRC")?],
            })
        }
        b"GRAY" => {
            // Grey arrives as R = G = B; equal thirds of white map it onto the grey axis.
            let white = xy_to_xyz(D50);
            let to_pcs = white.map(|component| [component / 3.0; 3]);
            let curve = read_curve(b"kTRC")?;
            Ok(Profile { to_pcs, curves: [curve.clone(), curve.clone(), curve] })
        }
        other => Err(DecodeError::unsupported(format!(
            "ICC colour space {}",
            String::from_utf8_lossy(other).trim()
        ))),
    }
}

fn tag_table(bytes: &[u8]) -> Result<Vec<Tag>, DecodeError> {
    let mut reader = ByteReader::new(bytes, Endian::Big).at(HEADER_LENGTH)?;
    let count = reader.u32()?;

    if count > MAX_TAGS {
        return Err(DecodeError::corrupt("too many ICC tags"));
    }
    (0..count)
        .map(|_| {
            let signature = reader.bytes(4)?.try_into().expect("four bytes were read");
            Ok(Tag { signature, offset: reader.u32()? as usize, length: reader.u32()? as usize })
        })
        .collect()
}

fn tag_data<'a>(bytes: &'a [u8], tag: &Tag) -> Result<ByteReader<'a>, DecodeError> {
    let data = ByteReader::new(bytes, Endian::Big).at(tag.offset)?.bytes(tag.length)?;
    Ok(ByteReader::new(data, Endian::Big))
}

fn fixed(reader: &mut ByteReader) -> Result<f64, DecodeError> {
    Ok(f64::from(reader.i32()?) / S15_FIXED16)
}

fn xyz(mut reader: ByteReader) -> Result<[f64; 3], DecodeError> {
    if reader.bytes(4)? != b"XYZ " {
        return Err(DecodeError::corrupt("ICC colorant is not an XYZ tag"));
    }
    reader.skip(4)?;
    Ok([fixed(&mut reader)?, fixed(&mut reader)?, fixed(&mut reader)?])
}

fn curve(mut reader: ByteReader) -> Result<Curve, DecodeError> {
    let kind = reader.bytes(4)?;
    reader.skip(4)?;

    match kind {
        b"curv" => {
            let count = reader.u32()? as usize;
            if count > reader.remaining() / 2 {
                return Err(DecodeError::corrupt("ICC curve is truncated"));
            }
            match count {
                0 => Ok(Curve::Linear),
                1 => Ok(Curve::Gamma(f32::from(reader.u16()?) / U8_FIXED8)),
                _ => {
                    let samples = (0..count).map(|_| Ok(f32::from(reader.u16()?) / 65535.0));
                    Ok(Curve::Table(samples.collect::<Result<_, DecodeError>>()?))
                }
            }
        }
        b"para" => {
            let function = reader.u16()?;
            reader.skip(2)?;
            let count = match function {
                0 => 1,
                1 => 3,
                2 => 4,
                3 => 5,
                4 => 7,
                _ => return Err(DecodeError::unsupported("ICC parametric curve type")),
            };
            let mut params = [0.0; 7];
            for value in params.iter_mut().take(count) {
                *value = fixed(&mut reader)? as f32;
            }
            Ok(Curve::Parametric { function, params })
        }
        _ => Err(DecodeError::unsupported("ICC curve encoding")),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn fixed_bytes(value: f64) -> [u8; 4] {
        ((value * S15_FIXED16).round() as i32).to_be_bytes()
    }

    /// Builds an RGB profile with the given colorants and a gamma-2.2 curve.
    pub(crate) fn build_rgb_profile(columns: [[f64; 3]; 3]) -> Vec<u8> {
        let mut tags: Vec<([u8; 4], Vec<u8>)> = Vec::new();
        for (signature, column) in [b"rXYZ", b"gXYZ", b"bXYZ"].into_iter().zip(columns) {
            let mut data = b"XYZ \0\0\0\0".to_vec();
            column.iter().for_each(|&v| data.extend(fixed_bytes(v)));
            tags.push((*signature, data));
        }
        for signature in [b"rTRC", b"gTRC", b"bTRC"] {
            tags.push((*signature, b"curv\0\0\0\0\0\0\0\x01\x02\x33".to_vec()));
        }
        let mut header = vec![0_u8; HEADER_LENGTH];
        header[16..20].copy_from_slice(b"RGB ");
        header[20..24].copy_from_slice(b"XYZ ");
        header[36..40].copy_from_slice(SIGNATURE);

        let mut table = (tags.len() as u32).to_be_bytes().to_vec();
        let mut data = Vec::new();
        let data_start = HEADER_LENGTH + 4 + tags.len() * 12;
        for (signature, bytes) in &tags {
            table.extend(signature);
            table.extend(((data_start + data.len()) as u32).to_be_bytes());
            table.extend((bytes.len() as u32).to_be_bytes());
            data.extend(bytes);
        }
        [header, table, data].concat()
    }

    #[test]
    fn reads_colorants_and_gamma() {
        let columns =
            [[0.4361, 0.2225, 0.0139], [0.3851, 0.7169, 0.0971], [0.1431, 0.0606, 0.7141]];
        let profile = parse(&build_rgb_profile(columns)).unwrap();
        assert!((profile.to_pcs[1][1] - 0.7169).abs() < 1e-4);
        assert_eq!(profile.curves[0], Curve::Gamma(563.0 / 256.0));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(&[0; 200]).is_err());
        assert!(parse(b"short").is_err());
    }
}
