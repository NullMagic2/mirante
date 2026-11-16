//! Module: mirante_format_heic::properties
//! Purpose: Parses the item properties stored in a HEIF `ipco` box.
//! Created: 2026-11-16
//! Architecture: Each property is decoded once into an owned value; items refer
//! to them by index through `ipma`. Unknown properties are kept by type only.

use crate::hevc::HvcConfig;
use mirante_codec::ByteReader;
use mirante_codec::isobmff::{FourCc, IsoBox, c_string};
use mirante_core::DecodeError;

/// Auxiliary type URNs that mark an item as the alpha plane of its master.
const ALPHA_URNS: [&str; 2] =
    ["urn:mpeg:hevc:2015:auxid:1", "urn:mpeg:mpegB:cicp:systems:auxiliary:alpha"];

#[derive(Clone, Debug, PartialEq)]
pub enum Property {
    /// `ispe`: the size of the reconstructed image.
    Size {
        width: u32,
        height: u32,
    },
    /// `colr` with an `nclx` payload.
    Nclx(Nclx),
    /// `colr` with a `prof` or `rICC` payload.
    Icc(Vec<u8>),
    Hevc(HvcConfig),
    /// `irot`: anticlockwise rotation in quarter turns.
    Rotation(u8),
    /// `imir`: axis 1 flips top to bottom, axis 0 left to right.
    Mirror {
        top_to_bottom: bool,
    },
    CleanAperture(CleanAperture),
    /// `pixi`: bits per channel.
    PixelBits(Vec<u8>),
    /// `auxC`: the auxiliary type URN.
    Auxiliary(String),
    /// `jpgC`: JPEG headers to put in front of every JPEG-coded item.
    JpegPrefix(Vec<u8>),
    Other(FourCc),
}

/// ITU-T H.273 colour description.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Nclx {
    pub primaries: u16,
    pub transfer: u16,
    pub matrix: u16,
    pub full_range: bool,
}

/// The `clap` box: rational size and centre offset of the visible area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CleanAperture {
    pub width: f64,
    pub height: f64,
    pub horizontal_offset: f64,
    pub vertical_offset: f64,
}

impl Property {
    pub fn parse(item: &IsoBox<'_>) -> Result<Self, DecodeError> {
        let mut reader = item.reader();
        Ok(match &item.box_type {
            b"ispe" => {
                let mut reader = item.full()?.reader();
                Self::Size { width: reader.u32()?, height: reader.u32()? }
            }
            b"colr" => parse_colour(&mut reader)?,
            b"hvcC" => Self::Hevc(HvcConfig::parse(item.payload)?),
            b"irot" => Self::Rotation(reader.u8()? & 0b11),
            b"imir" => Self::Mirror { top_to_bottom: reader.u8()? & 1 == 1 },
            b"clap" => Self::CleanAperture(parse_clean_aperture(&mut reader)?),
            b"pixi" => {
                let mut reader = item.full()?.reader();
                let channels = usize::from(reader.u8()?);
                Self::PixelBits(reader.bytes(channels)?.to_vec())
            }
            b"auxC" => Self::Auxiliary(c_string(&mut item.full()?.reader())?),
            b"jpgC" => Self::JpegPrefix(item.payload.to_vec()),
            other => Self::Other(*other),
        })
    }

    /// Clean aperture, rotation and mirroring change the image's geometry.
    pub fn is_transform(&self) -> bool {
        matches!(self, Self::CleanAperture(_) | Self::Rotation(_) | Self::Mirror { .. })
    }

    pub fn is_alpha(&self) -> bool {
        matches!(self, Self::Auxiliary(urn) if ALPHA_URNS.contains(&urn.as_str()))
    }
}

fn parse_colour(reader: &mut ByteReader<'_>) -> Result<Property, DecodeError> {
    let colour_type = reader.bytes(4)?;
    Ok(match colour_type {
        b"nclx" => Property::Nclx(Nclx {
            primaries: reader.u16()?,
            transfer: reader.u16()?,
            matrix: reader.u16()?,
            full_range: reader.u8()? & 0x80 != 0,
        }),
        b"prof" | b"rICC" => Property::Icc(reader.bytes(reader.remaining())?.to_vec()),
        _ => Property::Other(*b"colr"),
    })
}

fn parse_clean_aperture(reader: &mut ByteReader<'_>) -> Result<CleanAperture, DecodeError> {
    let mut fraction = |signed: bool| -> Result<f64, DecodeError> {
        let numerator = reader.u32()?;
        let denominator = reader.u32()?;
        let numerator = if signed { f64::from(numerator as i32) } else { f64::from(numerator) };
        if denominator == 0 {
            return Err(DecodeError::corrupt("clean aperture with a zero denominator"));
        }
        Ok(numerator / f64::from(denominator))
    };
    Ok(CleanAperture {
        width: fraction(false)?,
        height: fraction(false)?,
        horizontal_offset: fraction(true)?,
        vertical_offset: fraction(true)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(box_type: &[u8; 4], payload: &[u8]) -> Property {
        let item = IsoBox { box_type: *box_type, user_type: None, payload_offset: 0, payload };
        Property::parse(&item).unwrap()
    }

    #[test]
    fn parses_size_colour_and_geometry() {
        let ispe = [0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 200];
        assert_eq!(parse(b"ispe", &ispe), Property::Size { width: 256, height: 200 });
        let nclx = [b'n', b'c', b'l', b'x', 0, 12, 0, 13, 0, 1, 0x80];
        let expected = Nclx { primaries: 12, transfer: 13, matrix: 1, full_range: true };
        assert_eq!(parse(b"colr", &nclx), Property::Nclx(expected));
        assert_eq!(parse(b"colr", b"profICC!"), Property::Icc(b"ICC!".to_vec()));
        assert_eq!(parse(b"irot", &[3]), Property::Rotation(3));
        assert_eq!(parse(b"imir", &[1]), Property::Mirror { top_to_bottom: true });
    }

    #[test]
    fn parses_clean_aperture_with_negative_offset() {
        let mut clap = Vec::new();
        for value in [10_u32, 1, 6, 1, (-2_i32) as u32, 1, 1, 2] {
            clap.extend(value.to_be_bytes());
        }
        let Property::CleanAperture(clap) = parse(b"clap", &clap) else { panic!() };
        assert_eq!((clap.width, clap.height), (10.0, 6.0));
        assert_eq!((clap.horizontal_offset, clap.vertical_offset), (-2.0, 0.5));
    }

    #[test]
    fn recognises_alpha_auxiliaries() {
        let mut aux = vec![0; 4];
        aux.extend(b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha\0");
        assert!(parse(b"auxC", &aux).is_alpha());
        assert!(!parse(b"auxC", b"\0\0\0\0urn:depth\0").is_alpha());
    }
}
