//! Module: mirante_format_png::encode
//! Purpose: Writes an sRGB Image as a PNG file.
//! Created: 2026-11-16
//! Architecture: Built on mirante-codec primitives. Writes 16-bit RGBA, or RGB when
//! every pixel is opaque, marked sRGB; each row takes the filter whose output has
//! the smallest sum of absolute signed bytes, the heuristic libpng uses.

use crate::filter;
use crate::{SIGNATURE, chunk};
use mirante_codec::checksum::crc32_parts;
use mirante_codec::deflate::zlib_compress;
use mirante_core::{EncodeError, Image};

const BIT_DEPTH: u8 = 16;
const COLOR_TYPE_RGB: u8 = 2;
const COLOR_TYPE_RGBA: u8 = 6;
const RENDERING_INTENT_PERCEPTUAL: u8 = 0;
/// IDAT is split so no chunk grows unusually large for other readers.
const IDAT_CHUNK_BYTES: usize = 1 << 20;

pub(crate) fn write_chunk(output: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    output.extend_from_slice(&(data.len() as u32).to_be_bytes());
    output.extend_from_slice(kind);
    output.extend_from_slice(data);
    output.extend_from_slice(&crc32_parts(&[kind, data]).to_be_bytes());
}

fn to_u16(value: f32) -> u16 {
    // NaN clamps to 0 through the saturating cast.
    (value.clamp(0.0, 1.0) * f32::from(u16::MAX) + 0.5) as u16
}

/// Filters every row with its best filter and prefixes the filter type.
fn filter_rows(raw: &[u8], row_bytes: usize, bpp: usize) -> Vec<u8> {
    let mut output = Vec::with_capacity(raw.len() + raw.len() / row_bytes);
    let zeros = vec![0u8; row_bytes];
    let mut candidate = vec![0u8; row_bytes];
    let mut best = vec![0u8; row_bytes];

    for (index, row) in raw.chunks_exact(row_bytes).enumerate() {
        let previous =
            if index == 0 { &zeros[..] } else { &raw[(index - 1) * row_bytes..][..row_bytes] };
        let mut best_score = (u64::MAX, filter::NONE);
        for filter_type in filter::FILTERS {
            filter::apply(filter_type, row, previous, bpp, &mut candidate);
            let score = candidate.iter().map(|&byte| u64::from((byte as i8).unsigned_abs())).sum();
            if score < best_score.0 {
                best_score = (score, filter_type);
                std::mem::swap(&mut best, &mut candidate);
            }
        }
        output.push(best_score.1);
        output.extend_from_slice(&best);
    }
    output
}

pub fn encode(image: &Image) -> Result<Vec<u8>, EncodeError> {
    let opaque = image.pixels().iter().all(|pixel| to_u16(pixel[3]) == u16::MAX);
    let channels = if opaque { 3 } else { 4 };
    let row_bytes = image.width() as usize * channels * 2;
    let raw: Vec<u8> = image
        .pixels()
        .iter()
        .flat_map(|pixel| pixel[..channels].iter().flat_map(|&value| to_u16(value).to_be_bytes()))
        .collect();

    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&image.width().to_be_bytes());
    header.extend_from_slice(&image.height().to_be_bytes());
    let color_type = if opaque { COLOR_TYPE_RGB } else { COLOR_TYPE_RGBA };
    // Compression, filter method and interlace are all 0.
    header.extend_from_slice(&[BIT_DEPTH, color_type, 0, 0, 0]);

    let compressed = zlib_compress(&filter_rows(&raw, row_bytes, channels * 2));
    let mut output = SIGNATURE.to_vec();
    write_chunk(&mut output, chunk::IHDR, &header);
    write_chunk(&mut output, chunk::SRGB, &[RENDERING_INTENT_PERCEPTUAL]);
    for part in compressed.chunks(IDAT_CHUNK_BYTES) {
        write_chunk(&mut output, chunk::IDAT, part);
    }
    write_chunk(&mut output, chunk::IEND, &[]);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode;
    use mirante_core::ColorSpace;

    fn round_trip(pixels: Vec<[f32; 4]>, width: u32) -> (Vec<u8>, Image) {
        let height = pixels.len() as u32 / width;
        let image = Image::new(width, height, pixels, ColorSpace::Srgb).unwrap();
        let file = encode(&image).unwrap();
        (file.clone(), decode(&file).unwrap())
    }

    #[test]
    fn round_trips_opaque_and_transparent_images() {
        let quantise = |value: f32| f32::from(to_u16(value)) / 65535.0;
        let pixels: Vec<[f32; 4]> = (0..60)
            .map(|index| {
                let value = index as f32 / 59.0;
                [value, 1.0 - value, (value * 7.0).fract(), 1.0]
            })
            .collect();
        let (file, decoded) = round_trip(pixels.clone(), 6);
        assert_eq!(file[25], COLOR_TYPE_RGB);
        let expected: Vec<[f32; 4]> = pixels.iter().map(|pixel| pixel.map(quantise)).collect();
        assert_eq!(decoded.pixels(), expected);
        assert_eq!(decoded.color_space(), &ColorSpace::Srgb);

        let mut transparent = pixels;
        transparent[5][3] = 0.25;
        transparent[6] = [2.0, -1.0, f32::NAN, 0.0];
        let (file, decoded) = round_trip(transparent.clone(), 10);
        assert_eq!(file[25], COLOR_TYPE_RGBA);
        assert_eq!(decoded.pixels()[5], transparent[5].map(quantise));
        assert_eq!(decoded.pixels()[6], [1.0, 0.0, 0.0, 0.0]);
    }
}
