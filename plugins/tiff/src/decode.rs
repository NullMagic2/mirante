//! Module: mirante_format_tiff::decode
//! Purpose: Decodes the first full-resolution TIFF image into an Image.
//! Created: 2026-11-16
//! Architecture: layout.rs validates the IFD; each strip or tile is decompressed
//! here, split into samples (samples.rs) and turned into pixels (pixels.rs).
//! JPEG strips and tiles decode to pixels through the JPEG plugin (jpeg.rs).
//! Float samples are kept as stored and tagged like integer ones (ICC or sRGB).

use crate::layout::{JPEG_TABLES, Layout, compression, full_resolution_ifd, photometric};
use crate::samples::decode_row;
use crate::{jpeg, lzw, pixels};
use mirante_codec::tiff::{Ifd, Tiff, tag};
use mirante_codec::{inflate, packbits};
use mirante_core::{ColorSpace, DecodeError, Image, MAX_PIXELS};
use std::borrow::Cow;

/// One strip or tile: where it lands, its stored size and its compressed bytes.
pub struct Chunk<'a> {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub rows: u32,
    pub plane: usize,
    pub data: &'a [u8],
}

pub fn decode(bytes: &[u8]) -> Result<Image, DecodeError> {
    let tiff = Tiff::parse(bytes)?;
    let ifd = full_resolution_ifd(&tiff)?;
    let layout = Layout::read(&tiff, ifd)?;
    let chunks = chunks(&tiff, ifd, &layout)?;

    let pixels = if layout.compression == compression::JPEG {
        let tables = ifd.get(JPEG_TABLES).map(|entry| tiff.raw_bytes(entry)).transpose()?;
        jpeg::decode_chunks(&chunks, tables, &layout)?
    } else {
        let samples = read_samples(&chunks, &layout)?;
        let color_map = match layout.photometric {
            photometric::PALETTE => Some(layout.color_map(&tiff, ifd)?),
            _ => None,
        };
        pixels::convert(&samples, &layout, color_map.as_deref())
    };
    let orientation = tiff.get_u32(ifd, tag::ORIENTATION)?.unwrap_or(1);
    let image = Image::new(layout.width, layout.height, pixels, color_space(&tiff, ifd, &layout)?)?;
    Ok(image.oriented(orientation as u16))
}

/// CMYK profiles describe ink, not the RGB the naive conversion produces.
fn color_space(tiff: &Tiff, ifd: &Ifd, layout: &Layout) -> Result<ColorSpace, DecodeError> {
    match ifd.get(tag::ICC_PROFILE) {
        Some(entry) if layout.photometric != photometric::SEPARATED => {
            Ok(ColorSpace::Icc(tiff.raw_bytes(entry)?.into()))
        }
        _ => Ok(ColorSpace::Srgb),
    }
}

fn chunks<'a>(tiff: &Tiff<'a>, ifd: &Ifd, layout: &Layout) -> Result<Vec<Chunk<'a>>, DecodeError> {
    let tiled = ifd.get(tag::TILE_WIDTH).is_some();
    let (chunk_width, chunk_rows, offsets_tag, counts_tag) = if tiled {
        let size = |tag| tiff.get_u32(ifd, tag).map(Option::unwrap_or_default);
        (size(tag::TILE_WIDTH)?, size(tag::TILE_LENGTH)?, tag::TILE_OFFSETS, tag::TILE_BYTE_COUNTS)
    } else {
        let rows = tiff.get_u32(ifd, tag::ROWS_PER_STRIP)?.unwrap_or(u32::MAX);
        (layout.width, rows.min(layout.height), tag::STRIP_OFFSETS, tag::STRIP_BYTE_COUNTS)
    };
    if chunk_width == 0
        || chunk_rows == 0
        || u64::from(chunk_width) * u64::from(chunk_rows) > MAX_PIXELS
    {
        return Err(DecodeError::corrupt("bad TIFF strip or tile size"));
    }
    let list = |tag| {
        let entry = ifd
            .get(tag)
            .ok_or_else(|| DecodeError::corrupt("TIFF has no strip or tile offsets"))?;
        tiff.unsigned(entry)
    };
    let (offsets, counts) = (list(offsets_tag)?, list(counts_tag)?);
    let across = layout.width.div_ceil(chunk_width);
    let down = layout.height.div_ceil(chunk_rows);
    let planes = if layout.separate_planes { layout.samples_per_pixel } else { 1 };
    let needed = u64::from(across) * u64::from(down) * planes as u64;
    if (offsets.len().min(counts.len()) as u64) < needed {
        return Err(DecodeError::corrupt("TIFF lists too few strips or tiles"));
    }
    let mut chunks = Vec::with_capacity(needed as usize);
    for plane in 0..planes {
        for row in 0..down {
            for column in 0..across {
                let index = chunks.len();
                let (start, length) = (offsets[index] as usize, counts[index] as usize);
                let data = start
                    .checked_add(length)
                    .and_then(|end| tiff.data().get(start..end))
                    .ok_or_else(|| DecodeError::corrupt("TIFF strip lies outside the file"))?;
                let (x, y) = (column * chunk_width, row * chunk_rows);
                chunks.push(Chunk { x, y, width: chunk_width, rows: chunk_rows, plane, data });
            }
        }
    }
    Ok(chunks)
}

/// Gathers the colour and alpha samples of every pixel, `slots` values per pixel
/// (colour channels, then alpha); other extra samples are dropped here.
fn read_samples(chunks: &[Chunk], layout: &Layout) -> Result<Vec<f32>, DecodeError> {
    let slots = layout.color_channels + usize::from(layout.alpha.is_some());
    let slot_of = |sample: usize| match layout.alpha {
        _ if sample < layout.color_channels => Some(sample),
        Some(alpha) if alpha.sample == sample => Some(layout.color_channels),
        _ => None,
    };
    let width = layout.width as usize;
    let mut samples = vec![0.0; width * layout.height as usize * slots];

    for chunk in chunks {
        let per_pixel = layout.row.stride;
        let row_samples = chunk.width as usize * per_pixel;
        let row_bytes = layout.row.row_bytes(row_samples);
        let visible_rows = chunk.rows.min(layout.height - chunk.y) as usize;
        let visible_width = chunk.width.min(layout.width - chunk.x) as usize;
        let capacity = row_bytes * chunk.rows as usize;
        let data = decompress(chunk.data, layout, capacity, row_bytes * visible_rows)?;
        let targets: Vec<Option<usize>> = (0..per_pixel)
            .map(|index| slot_of(if layout.separate_planes { chunk.plane } else { index }))
            .collect();

        for (row, stored) in data.chunks_exact(row_bytes).take(visible_rows).enumerate() {
            let values = decode_row(stored, row_samples, &layout.row);
            let start = (chunk.y as usize + row) * width + chunk.x as usize;
            for (x, pixel) in values.chunks_exact(per_pixel).take(visible_width).enumerate() {
                let destination = (start + x) * slots;
                for (&value, target) in pixel.iter().zip(&targets) {
                    if let Some(slot) = target {
                        samples[destination + slot] = value;
                    }
                }
            }
        }
    }
    Ok(samples)
}

/// Returns at least `needed` bytes; `capacity` is the chunk's full stored size.
fn decompress<'a>(
    data: &'a [u8],
    layout: &Layout,
    capacity: usize,
    needed: usize,
) -> Result<Cow<'a, [u8]>, DecodeError> {
    let data: Cow<[u8]> = match layout.reversed_bits {
        true => data.iter().map(|byte| byte.reverse_bits()).collect(),
        false => Cow::Borrowed(data),
    };
    let output = match layout.compression {
        compression::NONE => data,
        compression::LZW => lzw::decompress(&data, capacity)?.into(),
        compression::DEFLATE | compression::DEFLATE_OLD => {
            inflate::zlib_decompress(&data, capacity)?.into()
        }
        compression::PACKBITS => packbits::unpack(&data, needed)?.into(),
        other => unreachable!("Layout::read rejects compression {other}"),
    };
    if output.len() < needed {
        return Err(DecodeError::corrupt("TIFF strip or tile is too short"));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::{Value, write_ifd};
    use crate::layout::{EXTRA_UNASSOCIATED_ALPHA, PLANAR_CHUNKY};
    use crate::lzw::tests::compress_literals;

    /// A little-endian TIFF whose single strip or tile data starts at offset 8.
    fn tiff(data: &[u8], mut entries: Vec<(u16, Value)>) -> Vec<u8> {
        let mut file = b"II*\0\0\0\0\0".to_vec();
        file.extend_from_slice(data);
        entries.sort_by_key(|(tag, _)| *tag);
        write_ifd(&mut file, &entries).unwrap();
        file
    }

    fn short(value: u16) -> Value {
        Value::Short(vec![value])
    }

    fn long(value: u32) -> Value {
        Value::Long(vec![value])
    }

    fn base(width: u32, height: u32, photometric: u16, data: &[u8]) -> Vec<(u16, Value)> {
        vec![
            (tag::IMAGE_WIDTH, long(width)),
            (tag::IMAGE_LENGTH, long(height)),
            (tag::PHOTOMETRIC, short(photometric)),
            (tag::STRIP_OFFSETS, long(8)),
            (tag::STRIP_BYTE_COUNTS, long(data.len() as u32)),
        ]
    }

    #[test]
    fn decodes_white_is_zero_bilevel_rotated() {
        let data = [0b1000_0000, 0b0100_0000];
        let mut entries = base(2, 2, photometric::WHITE_IS_ZERO, &data);
        entries.push((tag::ORIENTATION, short(6)));
        let image = decode(&tiff(&data, entries)).unwrap();
        // Stored rows [black, white], [white, black] turn a quarter clockwise.
        let grey: Vec<f32> = image.pixels().iter().map(|p| p[0]).collect();
        assert_eq!(grey, [1.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn decodes_lzw_rgba16_with_predictor() {
        let samples: [u16; 8] = [0, 1000, 65535, 65535, 500, 1000, 0, 32768];
        let mut differenced = samples;
        for index in (4..8).rev() {
            differenced[index] = samples[index].wrapping_sub(samples[index - 4]);
        }
        let raw: Vec<u8> = differenced.iter().flat_map(|v| v.to_le_bytes()).collect();
        let data = compress_literals(&raw);
        let mut entries = base(2, 1, photometric::RGB, &data);
        entries.extend([
            (tag::BITS_PER_SAMPLE, Value::Short(vec![16; 4])),
            (tag::COMPRESSION, short(compression::LZW)),
            (tag::SAMPLES_PER_PIXEL, short(4)),
            (tag::PLANAR_CONFIGURATION, short(PLANAR_CHUNKY)),
            (tag::PREDICTOR, short(2)),
            (tag::EXTRA_SAMPLES, short(EXTRA_UNASSOCIATED_ALPHA)),
        ]);
        let image = decode(&tiff(&data, entries)).unwrap();
        let expected = samples.map(|v| f32::from(v) / 65535.0);
        assert_eq!(image.pixels().as_flattened(), expected);
    }

    #[test]
    fn decodes_packbits_palette_tile_skipping_thumbnail() {
        // One 16x16 tile of 4-bit indices: index 1 everywhere (0x11 per byte).
        let data = [0xC1, 0x11];
        let mut map = vec![0_u16; 48];
        (map[1], map[17], map[33]) = (65535, 0, 32768);
        let entries = vec![
            (tag::IMAGE_WIDTH, long(3)),
            (tag::IMAGE_LENGTH, long(2)),
            (tag::BITS_PER_SAMPLE, short(4)),
            (tag::COMPRESSION, short(compression::PACKBITS)),
            (tag::PHOTOMETRIC, short(photometric::PALETTE)),
            (tag::COLOR_MAP, Value::Short(map)),
            (tag::TILE_WIDTH, long(16)),
            (tag::TILE_LENGTH, long(16)),
            (tag::TILE_OFFSETS, long(8)),
            (tag::TILE_BYTE_COUNTS, long(data.len() as u32)),
        ];
        let mut file = tiff(&data, entries);
        // Put a reduced-resolution IFD first that chains to the real one.
        let real = u32::from_le_bytes(file[4..8].try_into().unwrap());
        let thumbnail = file.len() as u32;
        file.extend(1_u16.to_le_bytes());
        file.extend([0xFE, 0, 4, 0, 1, 0, 0, 0, 1, 0, 0, 0]);
        file.extend(real.to_le_bytes());
        file[4..8].copy_from_slice(&thumbnail.to_le_bytes());
        let image = decode(&file).unwrap();
        assert_eq!((image.width(), image.height()), (3, 2));
        assert_eq!(image.pixels()[5], [1.0, 0.0, 32768.0 / 65535.0, 1.0]);
    }

    #[test]
    fn rejects_short_strips_and_plain_ycbcr() {
        // Two rows of 1-bit samples need two bytes.
        let data = [0];
        let file = tiff(&data, base(2, 2, photometric::BLACK_IS_ZERO, &data));
        assert!(matches!(decode(&file), Err(DecodeError::Corrupt(_))));
        let file = tiff(&data, base(1, 1, photometric::YCBCR, &data));
        assert!(matches!(decode(&file), Err(DecodeError::Unsupported(_))));
    }
}
