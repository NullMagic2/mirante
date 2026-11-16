//! Module: mirante_format_tiff::encode
//! Purpose: Writes an sRGB Image as a 16-bit, Deflate-compressed TIFF file.
//! Created: 2026-11-16
//! Architecture: Little-endian, chunky RGB (or RGBA with unassociated alpha when
//! any pixel is translucent), horizontal predictor, strips of about 1 MiB.

use crate::layout::{EXTRA_UNASSOCIATED_ALPHA, PLANAR_CHUNKY, compression, photometric};
use crate::samples::PREDICTOR_HORIZONTAL;
use mirante_codec::deflate::zlib_compress;
use mirante_codec::tiff::tag;
use mirante_core::{EncodeError, Image};

const STRIP_BYTES: usize = 1 << 20;
const BITS: u16 = 16;
const HEADER_BYTES: usize = 8;
const ENTRY_BYTES: usize = 12;
const FIELD_SHORT: u16 = 3;
const FIELD_LONG: u16 = 4;

pub(crate) enum Value {
    Short(Vec<u16>),
    Long(Vec<u32>),
}

impl Value {
    /// The TIFF field type, the value count and the little-endian value bytes.
    fn encoded(&self) -> (u16, u32, Vec<u8>) {
        match self {
            Self::Short(v) => {
                (FIELD_SHORT, v.len() as u32, v.iter().flat_map(|v| v.to_le_bytes()).collect())
            }
            Self::Long(v) => {
                (FIELD_LONG, v.len() as u32, v.iter().flat_map(|v| v.to_le_bytes()).collect())
            }
        }
    }
}

pub fn encode(image: &Image) -> Result<Vec<u8>, EncodeError> {
    let (width, height) = (image.width() as usize, image.height() as usize);
    let opaque = image.pixels().iter().all(|pixel| pixel[3] >= 1.0);
    let channels = if opaque { 3 } else { 4 };
    let row_bytes = width * channels * 2;
    let rows_per_strip = (STRIP_BYTES / row_bytes).clamp(1, height);

    let mut file = Vec::from(*b"II*\0");
    file.extend([0; 4]);
    let (mut offsets, mut counts) = (Vec::new(), Vec::new());
    for first_row in (0..height).step_by(rows_per_strip) {
        let rows = first_row..(first_row + rows_per_strip).min(height);
        let mut strip = Vec::with_capacity(rows.len() * row_bytes);
        for y in rows {
            strip.extend(predicted_row(image, y as u32, channels));
        }
        let compressed = zlib_compress(&strip);
        offsets.push(file_offset(file.len())?);
        counts.push(file_offset(compressed.len())?);
        file.extend(compressed);
    }

    let mut entries = vec![
        (tag::IMAGE_WIDTH, Value::Long(vec![image.width()])),
        (tag::IMAGE_LENGTH, Value::Long(vec![image.height()])),
        (tag::BITS_PER_SAMPLE, Value::Short(vec![BITS; channels])),
        (tag::COMPRESSION, Value::Short(vec![compression::DEFLATE])),
        (tag::PHOTOMETRIC, Value::Short(vec![photometric::RGB])),
        (tag::STRIP_OFFSETS, Value::Long(offsets)),
        (tag::SAMPLES_PER_PIXEL, Value::Short(vec![channels as u16])),
        (tag::ROWS_PER_STRIP, Value::Long(vec![rows_per_strip as u32])),
        (tag::STRIP_BYTE_COUNTS, Value::Long(counts)),
        (tag::PLANAR_CONFIGURATION, Value::Short(vec![PLANAR_CHUNKY])),
        (tag::PREDICTOR, Value::Short(vec![PREDICTOR_HORIZONTAL])),
    ];
    if !opaque {
        entries.push((tag::EXTRA_SAMPLES, Value::Short(vec![EXTRA_UNASSOCIATED_ALPHA])));
    }
    write_ifd(&mut file, &entries)?;
    Ok(file)
}

/// One row of 16-bit samples, each stored as its difference from the same
/// channel of the previous pixel.
fn predicted_row(image: &Image, y: u32, channels: usize) -> Vec<u8> {
    let samples: Vec<u16> = image
        .row(y)
        .iter()
        .flat_map(|pixel| {
            pixel[..channels].iter().map(|&v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16)
        })
        .collect();
    let mut row = Vec::with_capacity(samples.len() * 2);
    for (index, &sample) in samples.iter().enumerate() {
        let previous = if index >= channels { samples[index - channels] } else { 0 };
        row.extend(sample.wrapping_sub(previous).to_le_bytes());
    }
    row
}

/// Appends the IFD (word aligned) and its out-of-line values, then points the
/// header at it. Entries must be in ascending tag order.
pub(crate) fn write_ifd(file: &mut Vec<u8>, entries: &[(u16, Value)]) -> Result<(), EncodeError> {
    if file.len() % 2 == 1 {
        file.push(0);
    }
    let ifd_offset = file.len();
    let mut extra_offset = ifd_offset + 2 + entries.len() * ENTRY_BYTES + 4;
    let mut extra = Vec::new();
    file.extend((entries.len() as u16).to_le_bytes());

    for (tag, value) in entries {
        let (field_type, count, mut bytes) = value.encoded();
        file.extend(tag.to_le_bytes());
        file.extend(field_type.to_le_bytes());
        file.extend(count.to_le_bytes());
        if bytes.len() <= 4 {
            bytes.resize(4, 0);
            file.extend(bytes);
        } else {
            file.extend(file_offset(extra_offset)?.to_le_bytes());
            extra_offset += bytes.len() + bytes.len() % 2;
            if bytes.len() % 2 == 1 {
                bytes.push(0);
            }
            extra.extend(bytes);
        }
    }
    file.extend(0_u32.to_le_bytes());
    file.extend(extra);
    file_offset(file.len())?;
    file[4..HEADER_BYTES].copy_from_slice(&file_offset(ifd_offset)?.to_le_bytes());
    Ok(())
}

fn file_offset(offset: usize) -> Result<u32, EncodeError> {
    u32::try_from(offset).map_err(|_| EncodeError("image is too large for a TIFF file".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante_core::ColorSpace;

    #[test]
    fn round_trips_translucent_pixels() {
        let pixels = vec![[1.0, 0.5, 0.0, 1.0], [0.25, 0.0, 1.0, 0.5], [0.0, 0.0, 0.0, 0.0]];
        let image = Image::new(3, 1, pixels.clone(), ColorSpace::Srgb).unwrap();
        let decoded = crate::decode(&encode(&image).unwrap()).unwrap();
        for (got, want) in decoded.pixels().iter().zip(&pixels) {
            for (g, w) in got.iter().zip(want) {
                assert!((g - w).abs() < 1e-4, "{got:?} != {want:?}");
            }
        }
    }

    #[test]
    fn writes_rgb_when_opaque() {
        let image = Image::new(2, 2, vec![[0.2, 0.4, 0.6, 1.0]; 4], ColorSpace::Srgb).unwrap();
        let decoded = crate::decode(&encode(&image).unwrap()).unwrap();
        assert!((decoded.pixels()[3][2] - 0.6).abs() < 1e-4);
    }
}
