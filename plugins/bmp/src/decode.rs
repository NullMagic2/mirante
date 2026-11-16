//! Module: mirante_format_bmp::decode
//! Purpose: Decodes BMP files into an Image in the file's colour space.
//! Created: 2026-11-16
//! Architecture: Reads the OS/2 core, INFO, V4 and V5 headers into one Header,
//! then expands palette, bit-field or RLE (rle.rs) pixel data. Only V5 files can
//! embed an ICC profile; everything else is sRGB.

use crate::rle;
use mirante_codec::{ByteReader, Endian};
use mirante_core::{ColorSpace, DecodeError, Image, Rgba};
use std::ops::Range;

const SIGNATURE: &[u8] = b"BM";
const FILE_HEADER_BYTES: usize = 14;
const CORE_HEADER: u32 = 12;
const INFO_HEADER: u32 = 40;
const OS2_V2_HEADER: u32 = 64;
const V4_HEADER: u32 = 108;
const V5_HEADER: u32 = 124;
/// Adobe's V2/V3 headers: INFO plus the RGB, or RGBA, masks.
const MASKS_IN_HEADER: u32 = 52;
const ALPHA_MASK_IN_HEADER: u32 = 56;
/// Offsets within the header of the V4 colour-space type and V5 profile fields.
const COLOR_SPACE_TYPE_AT: usize = 56;
const PROFILE_AT: usize = 112;
/// LCS 'MBED', as the little-endian integer Windows stores.
const PROFILE_EMBEDDED: u32 = 0x4D42_4544;

mod compression {
    pub const RGB: u32 = 0;
    pub const RLE8: u32 = 1;
    pub const RLE4: u32 = 2;
    pub const BITFIELDS: u32 = 3;
    pub const JPEG: u32 = 4;
    pub const PNG: u32 = 5;
    pub const ALPHA_BITFIELDS: u32 = 6;
}

const MASKS_16: [u32; 4] = [0x7C00, 0x03E0, 0x001F, 0];
const MASKS_32: [u32; 4] = [0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0xFF00_0000];

struct Header {
    width: u32,
    height: u32,
    top_down: bool,
    bits: u16,
    compression: u32,
    colors_used: u32,
    /// Red, green, blue and alpha masks for 16- and 32-bit pixels.
    masks: [u32; 4],
    /// Where the palette starts and ends, and how wide its entries are (3 or 4 bytes).
    palette_at: usize,
    palette_end: usize,
    palette_entry: usize,
    profile: Option<Range<usize>>,
}

pub fn decode(bytes: &[u8]) -> Result<Image, DecodeError> {
    let mut reader = ByteReader::new(bytes, Endian::Little);
    if reader.bytes(SIGNATURE.len())? != SIGNATURE {
        return Err(DecodeError::corrupt("not a BMP file"));
    }
    reader.skip(8)?;
    let data_at = reader.u32()? as usize;
    let mut header = read_header(&mut reader)?;
    if data_at > header.palette_at {
        header.palette_end = data_at;
    }
    let (width, height) = (header.width as usize, header.height as usize);
    let pixel_data =
        bytes.get(data_at..).ok_or_else(|| DecodeError::corrupt("BMP pixel data offset"))?;

    let stored_rows = match header.compression {
        compression::RLE8 | compression::RLE4 => {
            let four_bit = header.compression == compression::RLE4;
            if header.bits != if four_bit { 4 } else { 8 } {
                return Err(DecodeError::corrupt("BMP RLE with the wrong bit depth"));
            }
            let palette = palette(bytes, &header)?;
            let indices = rle::expand(pixel_data, width, height, four_bit);
            indices
                .into_iter()
                .map(|index| match index {
                    Some(index) => lookup(&palette, index),
                    None => Ok([0.0; 4]),
                })
                .collect::<Result<Vec<_>, _>>()?
        }
        _ => uncompressed(bytes, pixel_data, &header)?,
    };
    let pixels = if header.top_down {
        stored_rows
    } else {
        stored_rows.chunks_exact(width).rev().flatten().copied().collect()
    };
    let color_space = match header.profile {
        Some(range) => {
            let profile =
                bytes.get(range).ok_or_else(|| DecodeError::corrupt("BMP ICC profile"))?;
            ColorSpace::Icc(profile.into())
        }
        None => ColorSpace::Srgb,
    };
    Ok(Image::new(header.width, height as u32, pixels, color_space)?)
}

fn read_header(reader: &mut ByteReader) -> Result<Header, DecodeError> {
    let header_size = reader.u32()?;
    let header_end = FILE_HEADER_BYTES + header_size as usize;
    if header_size == CORE_HEADER {
        let (width, height) = (u32::from(reader.u16()?), u32::from(reader.u16()?));
        reader.skip(2)?;
        let bits = reader.u16()?;
        DecodeError::check_size(width, height)?;
        return Ok(Header {
            width,
            height,
            top_down: false,
            bits,
            compression: compression::RGB,
            colors_used: 0,
            masks: default_masks(bits),
            palette_at: header_end,
            palette_end: usize::MAX,
            palette_entry: 3,
            profile: None,
        });
    }
    if !matches!(header_size, INFO_HEADER | MASKS_IN_HEADER | ALPHA_MASK_IN_HEADER | OS2_V2_HEADER)
        && header_size < V4_HEADER
    {
        return Err(DecodeError::unsupported(format!("BMP header of {header_size} bytes")));
    }
    let header = reader.bytes(header_size as usize - 4)?;
    let mut fields = ByteReader::new(header, Endian::Little);
    let width = fields.i32()?;
    let height = fields.i32()?;
    fields.skip(2)?;
    let bits = fields.u16()?;
    let compression = fields.u32()?;
    fields.skip(12)?;
    let colors_used = fields.u32()?;
    fields.skip(4)?;

    if width <= 0 || height == 0 || height == i32::MIN {
        return Err(DecodeError::corrupt(format!("BMP size {width}x{height}")));
    }
    let top_down = height < 0;
    let (width, height) = (width as u32, height.unsigned_abs());
    DecodeError::check_size(width, height)?;
    if header_size == OS2_V2_HEADER && compression > compression::RLE4 {
        return Err(DecodeError::unsupported("OS/2 BMP Huffman or RLE24 compression"));
    }
    // INFO headers keep bit-field masks just after the header, before the palette.
    let trailing_masks = match (header_size, compression) {
        (INFO_HEADER, compression::BITFIELDS) => 3,
        (INFO_HEADER, compression::ALPHA_BITFIELDS) => 4,
        _ => 0,
    };
    let masks_in_header = match header_size {
        MASKS_IN_HEADER => 3,
        OS2_V2_HEADER => 0,
        size if size >= ALPHA_MASK_IN_HEADER => 4,
        _ => 0,
    };
    let mut masks = default_masks(bits);
    if matches!(compression, compression::BITFIELDS | compression::ALPHA_BITFIELDS) {
        let (mut source, count) = match trailing_masks {
            0 => (fields.clone(), masks_in_header),
            count => (reader.clone(), count),
        };
        masks = [0; 4];
        for mask in masks.iter_mut().take(count) {
            *mask = source.u32()?;
        }
    }
    let profile = match header_size {
        size if size >= V5_HEADER => embedded_profile(header)?,
        _ => None,
    };
    Ok(Header {
        width,
        height,
        top_down,
        bits,
        compression,
        colors_used,
        masks,
        palette_at: header_end + trailing_masks * 4,
        palette_end: usize::MAX,
        palette_entry: 4,
        profile,
    })
}

/// The file range of a V5 embedded profile; `header` starts after its size
/// field, and the stored offset counts from the start of the header.
fn embedded_profile(header: &[u8]) -> Result<Option<Range<usize>>, DecodeError> {
    let mut fields = ByteReader::new(header, Endian::Little).at(COLOR_SPACE_TYPE_AT - 4)?;
    if fields.u32()? != PROFILE_EMBEDDED {
        return Ok(None);
    }
    let mut fields = fields.at(PROFILE_AT - 4)?;
    let (offset, size) = (fields.u32()? as usize, fields.u32()? as usize);
    let start = FILE_HEADER_BYTES + offset;
    let end = start.checked_add(size).ok_or_else(|| DecodeError::corrupt("BMP ICC profile"))?;
    Ok((size > 0).then_some(start..end))
}

fn default_masks(bits: u16) -> [u32; 4] {
    if bits == 16 { MASKS_16 } else { MASKS_32 }
}

fn palette(bytes: &[u8], header: &Header) -> Result<Vec<Rgba>, DecodeError> {
    let capacity = 1_usize << header.bits.min(8);
    let count = match header.colors_used as usize {
        0 => capacity,
        used => used.min(capacity),
    };
    let end = header.palette_end.min(bytes.len());
    let entries = bytes.get(header.palette_at..end).unwrap_or_default();
    Ok(entries
        .chunks_exact(header.palette_entry)
        .take(count)
        .map(|bgr| [bgr[2], bgr[1], bgr[0]].map(|v| f32::from(v) / 255.0))
        .map(|[r, g, b]| [r, g, b, 1.0])
        .collect())
}

fn lookup(palette: &[Rgba], index: u8) -> Result<Rgba, DecodeError> {
    palette
        .get(usize::from(index))
        .copied()
        .ok_or_else(|| DecodeError::corrupt("BMP palette index out of range"))
}

/// Uncompressed and bit-field pixel data, rows in stored order.
fn uncompressed(bytes: &[u8], data: &[u8], header: &Header) -> Result<Vec<Rgba>, DecodeError> {
    use compression::*;
    let bits = header.bits;
    let fits = match header.compression {
        RGB => matches!(bits, 1 | 2 | 4 | 8 | 16 | 24 | 32),
        BITFIELDS | ALPHA_BITFIELDS => matches!(bits, 16 | 32),
        JPEG | PNG => return Err(DecodeError::unsupported("BMP with embedded JPEG or PNG")),
        _ => false,
    };
    if !fits {
        let compression = header.compression;
        return Err(DecodeError::unsupported(format!("{bits}-bit BMP, compression {compression}")));
    }
    let width = header.width as usize;
    let stride = (width * usize::from(bits)).div_ceil(32) * 4;
    let size = stride * header.height as usize;
    let data =
        data.get(..size).ok_or_else(|| DecodeError::corrupt("BMP pixel data is truncated"))?;
    let mut pixels = Vec::with_capacity(width * header.height as usize);

    match bits {
        1 | 2 | 4 | 8 => {
            let palette = palette(bytes, header)?;
            let per_byte = 8 / usize::from(bits);
            let mask = ((1_u32 << bits) - 1) as u8;
            for row in data.chunks_exact(stride) {
                for x in 0..width {
                    let shift = 8 - usize::from(bits) * (x % per_byte + 1);
                    pixels.push(lookup(&palette, (row[x / per_byte] >> shift) & mask)?);
                }
            }
        }
        24 => {
            for row in data.chunks_exact(stride) {
                let bgr = row.chunks_exact(3).take(width);
                pixels.extend(bgr.map(|p| [p[2], p[1], p[0], 255].map(|v| f32::from(v) / 255.0)));
            }
        }
        _ => {
            let fields = header.masks.map(Field::new);
            let bytes_per_pixel = usize::from(bits) / 8;
            for row in data.chunks_exact(stride) {
                for pixel in row.chunks_exact(bytes_per_pixel).take(width) {
                    let value =
                        pixel.iter().rev().fold(0, |value, &byte| value << 8 | u32::from(byte));
                    pixels.push(fields.map(|field| field.map_or(0.0, |f| f.value(value))));
                }
            }
            // Many writers leave the alpha byte zero; such images are opaque.
            if fields[3].is_none() || pixels.iter().all(|pixel| pixel[3] == 0.0) {
                pixels.iter_mut().for_each(|pixel| pixel[3] = 1.0);
            }
        }
    }
    Ok(pixels)
}

/// One bit-field channel: its mask, how far to shift it down and its maximum.
#[derive(Clone, Copy)]
struct Field {
    mask: u32,
    shift: u32,
    max: f32,
}

impl Field {
    fn new(mask: u32) -> Option<Self> {
        let shift = mask.trailing_zeros();
        (mask != 0).then(|| Self { mask, shift, max: (mask >> shift) as f32 })
    }

    fn value(&self, pixel: u32) -> f32 {
        ((pixel & self.mask) >> self.shift) as f32 / self.max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A BMP with an INFO header; `extra` (masks or palette) sits before the pixels.
    fn bmp(
        width: i32,
        height: i32,
        bits: u16,
        compression: u32,
        extra: &[u8],
        data: &[u8],
    ) -> Vec<u8> {
        let data_at = (FILE_HEADER_BYTES + INFO_HEADER as usize + extra.len()) as u32;
        let mut file = b"BM".to_vec();
        file.extend((data_at + data.len() as u32).to_le_bytes());
        file.extend([0; 4]);
        file.extend(data_at.to_le_bytes());
        file.extend(INFO_HEADER.to_le_bytes());
        file.extend(width.to_le_bytes());
        file.extend(height.to_le_bytes());
        file.extend(1_u16.to_le_bytes());
        file.extend(bits.to_le_bytes());
        file.extend(compression.to_le_bytes());
        file.extend([0; 20]);
        file.extend_from_slice(extra);
        file.extend_from_slice(data);
        file
    }

    #[test]
    fn decodes_bottom_up_24_bit_rows() {
        // Stored bottom row first: blue, then red; each row padded to 4 bytes.
        let data = [255, 0, 0, 0, 0, 0, 255, 0];
        let image = decode(&bmp(1, 2, 24, compression::RGB, &[], &data)).unwrap();
        assert_eq!(image.pixels(), [[1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]]);
    }

    #[test]
    fn zero_alpha_is_opaque_but_real_alpha_is_kept() {
        let opaque = decode(&bmp(1, 1, 32, compression::RGB, &[], &[0, 0, 255, 0])).unwrap();
        assert_eq!(opaque.pixels(), [[1.0, 0.0, 0.0, 1.0]]);
        let masks: Vec<u8> = [0xFF_u32, 0xFF00, 0xFF_0000, 0xFF00_0000]
            .iter()
            .flat_map(|m| m.to_le_bytes())
            .collect();
        let file = bmp(1, -1, 32, compression::ALPHA_BITFIELDS, &masks, &[255, 0, 0, 51]);
        assert_eq!(decode(&file).unwrap().pixels(), [[1.0, 0.0, 0.0, 0.2]]);
    }

    #[test]
    fn rle_skipped_pixels_are_transparent() {
        let palette = [0, 0, 0, 0, 255, 255, 255, 0];
        // One white pixel, then end of bitmap.
        let file = bmp(2, 1, 8, compression::RLE8, &palette, &[1, 1, 0, 1]);
        let image = decode(&file).unwrap();
        assert_eq!(image.pixels(), [[1.0, 1.0, 1.0, 1.0], [0.0; 4]]);
    }

    #[test]
    fn rejects_embedded_jpeg_and_truncated_pixels() {
        let jpeg = bmp(1, 1, 0, compression::JPEG, &[], &[0xFF, 0xD8]);
        assert!(matches!(decode(&jpeg), Err(DecodeError::Unsupported(_))));
        let short = bmp(2, 2, 24, compression::RGB, &[], &[0; 9]);
        assert!(matches!(decode(&short), Err(DecodeError::Corrupt(_))));
    }
}
