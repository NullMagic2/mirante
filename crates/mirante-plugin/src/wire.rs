//! Module: mirante_plugin::wire
//! Purpose: The image format plugins and the viewer exchange over pipes.
//! Created: 2026-11-16
//! Architecture: "MIRI": a 20-byte header, the ICC profile if any, then RGBA
//! samples. Encoded colour in 0..1 travels as u16 (half the bytes); linear or
//! out-of-range data travels as f32 so nothing is clipped. Little-endian.

use mirante_core::{ColorSpace, DecodeError, Image};
use std::io::{self, Read, Write};

const MAGIC: &[u8; 4] = b"MIRI";
const VERSION: u8 = 1;
const SAMPLES_U16: u8 = 1;
const SAMPLES_F32: u8 = 2;
const MAX_ICC: u32 = 64 << 20;

const SPACE_SRGB: u8 = 0;
const SPACE_LINEAR_SRGB: u8 = 1;
const SPACE_LINEAR_REC2020: u8 = 2;
const SPACE_ICC: u8 = 3;

pub fn write_image(image: &Image, out: &mut impl Write) -> io::Result<()> {
    let (space, icc): (u8, &[u8]) = match image.color_space() {
        ColorSpace::Srgb => (SPACE_SRGB, &[]),
        ColorSpace::LinearSrgb => (SPACE_LINEAR_SRGB, &[]),
        ColorSpace::LinearRec2020 => (SPACE_LINEAR_REC2020, &[]),
        ColorSpace::Icc(bytes) => (SPACE_ICC, bytes),
    };
    let linear = matches!(space, SPACE_LINEAR_SRGB | SPACE_LINEAR_REC2020);
    let in_range = image.pixels().iter().flatten().all(|v| (0.0..=1.0).contains(v));
    let samples = if linear || !in_range { SAMPLES_F32 } else { SAMPLES_U16 };

    let mut header = Vec::with_capacity(20 + icc.len());
    header.extend(MAGIC);
    header.extend([VERSION, samples, space, 0]);
    header.extend(image.width().to_le_bytes());
    header.extend(image.height().to_le_bytes());
    header.extend((icc.len() as u32).to_le_bytes());
    header.extend(icc);
    out.write_all(&header)?;

    let body: Vec<u8> = if samples == SAMPLES_U16 {
        let to_u16 = |v: f32| (v * f32::from(u16::MAX)).round() as u16;
        image.pixels().iter().flatten().flat_map(|&v| to_u16(v).to_le_bytes()).collect()
    } else {
        image.pixels().iter().flatten().flat_map(|v| v.to_le_bytes()).collect()
    };
    out.write_all(&body)?;
    out.flush()
}

fn io_error(error: io::Error) -> DecodeError {
    DecodeError::corrupt(format!("plugin output: {error}"))
}

pub fn read_image(input: &mut (impl Read + ?Sized)) -> Result<Image, DecodeError> {
    let mut header = [0_u8; 20];
    input.read_exact(&mut header).map_err(io_error)?;
    if &header[..4] != MAGIC || header[4] != VERSION {
        return Err(DecodeError::corrupt("plugin output is not a MIRI image"));
    }
    let word = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().expect("four bytes"));
    let (samples, space) = (header[5], header[6]);
    let (width, height, icc_length) = (word(8), word(12), word(16));
    let count = DecodeError::check_size(width, height)?;

    if icc_length > MAX_ICC {
        return Err(DecodeError::corrupt("plugin sent an oversized ICC profile"));
    }
    let mut icc = vec![0; icc_length as usize];
    input.read_exact(&mut icc).map_err(io_error)?;
    let color_space = match space {
        SPACE_SRGB => ColorSpace::Srgb,
        SPACE_LINEAR_SRGB => ColorSpace::LinearSrgb,
        SPACE_LINEAR_REC2020 => ColorSpace::LinearRec2020,
        SPACE_ICC => ColorSpace::Icc(icc.into()),
        _ => return Err(DecodeError::corrupt("plugin sent an unknown colour space")),
    };

    let sample_bytes = match samples {
        SAMPLES_U16 => 2,
        SAMPLES_F32 => 4,
        _ => return Err(DecodeError::corrupt("plugin sent an unknown sample type")),
    };
    let mut body = vec![0_u8; count * 4 * sample_bytes];
    input.read_exact(&mut body).map_err(io_error)?;
    let values: Vec<f32> = if samples == SAMPLES_U16 {
        body.chunks_exact(2)
            .map(|b| f32::from(u16::from_le_bytes([b[0], b[1]])) / f32::from(u16::MAX))
            .collect()
    } else {
        body.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
    };
    let pixels = values.chunks_exact(4).map(|p| [p[0], p[1], p[2], p[3]]).collect();
    Ok(Image::new(width, height, pixels, color_space)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(image: &Image) -> Image {
        let mut bytes = Vec::new();
        write_image(image, &mut bytes).unwrap();
        read_image(&mut bytes.as_slice()).unwrap()
    }

    #[test]
    fn encoded_colour_round_trips_through_u16() {
        let icc = ColorSpace::Icc(vec![1, 2, 3].into());
        let image = Image::new(2, 1, vec![[0.0, 0.5, 1.0, 1.0], [0.25; 4]], icc).unwrap();
        let back = round_trip(&image);
        assert_eq!(back.color_space(), image.color_space());
        let pairs = back.pixels().iter().flatten().zip(image.pixels().iter().flatten());
        assert!(pairs.clone().all(|(a, b)| (a - b).abs() < 1e-4));
    }

    #[test]
    fn linear_values_above_one_survive() {
        let image =
            Image::new(1, 1, vec![[3.5, -0.1, 0.2, 1.0]], ColorSpace::LinearRec2020).unwrap();
        assert_eq!(round_trip(&image).pixels(), image.pixels());
    }

    #[test]
    fn short_streams_are_errors() {
        let image = Image::new(1, 1, vec![[0.0; 4]], ColorSpace::Srgb).unwrap();
        let mut bytes = Vec::new();
        write_image(&image, &mut bytes).unwrap();
        assert!(read_image(&mut &bytes[..bytes.len() - 1]).is_err());
    }
}
