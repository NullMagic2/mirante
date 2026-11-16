//! Module: mirante_format_png::decode
//! Purpose: Decodes PNG files into an Image in the file's colour space.
//! Created: 2026-11-16
//! Architecture: Built on mirante-codec primitives. Chunks are parsed and checked
//! first, then the joined IDAT stream is inflated, unfiltered pass by pass and
//! expanded to RGBA floats. APNG frames are ignored; the default image is shown.

use crate::filter;
use crate::{SIGNATURE, chunk};
use mirante_codec::checksum::crc32_parts;
use mirante_codec::inflate::zlib_decompress;
use mirante_codec::{ByteReader, Endian};
use mirante_core::{ColorSpace, DecodeError, Image, Rgba};
use std::sync::Arc;

/// PNG limits chunk lengths and dimensions to 2^31 - 1.
const MAX_PNG_INTEGER: u32 = 0x7FFF_FFFF;
/// Decompressed ICC profiles larger than this are refused as implausible.
const MAX_ICC_BYTES: usize = 64 << 20;
const COMPRESSION_DEFLATE: u8 = 0;
const INTERLACE_ADAM7: u8 = 1;
/// Ancillary chunks have bit 5 set in the first type byte (lowercase letter).
const ANCILLARY_BIT: u8 = 0x20;
/// Adam7 passes as (x start, y start, x step, y step).
const ADAM7: [(usize, usize, usize, usize); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum ColorType {
    Gray,
    Rgb,
    Palette,
    GrayAlpha,
    Rgba,
}

impl ColorType {
    fn from_code(code: u8) -> Result<Self, DecodeError> {
        Ok(match code {
            0 => Self::Gray,
            2 => Self::Rgb,
            3 => Self::Palette,
            4 => Self::GrayAlpha,
            6 => Self::Rgba,
            _ => return Err(DecodeError::corrupt(format!("unknown PNG colour type {code}"))),
        })
    }

    fn channels(self) -> usize {
        match self {
            Self::Gray | Self::Palette => 1,
            Self::GrayAlpha => 2,
            Self::Rgb => 3,
            Self::Rgba => 4,
        }
    }

    fn allows_depth(self, depth: u8) -> bool {
        match self {
            Self::Gray => matches!(depth, 1 | 2 | 4 | 8 | 16),
            Self::Palette => matches!(depth, 1 | 2 | 4 | 8),
            _ => matches!(depth, 8 | 16),
        }
    }
}

struct Header {
    width: u32,
    height: u32,
    depth: u8,
    color_type: ColorType,
    interlaced: bool,
}

impl Header {
    fn parse(data: &[u8]) -> Result<Self, DecodeError> {
        if data.len() != 13 {
            return Err(DecodeError::corrupt("PNG header has the wrong length"));
        }
        let mut reader = ByteReader::new(data, Endian::Big);
        let (width, height) = (reader.u32()?, reader.u32()?);
        let (depth, color_type) = (reader.u8()?, ColorType::from_code(reader.u8()?)?);
        let (compression, filter_method, interlace) = (reader.u8()?, reader.u8()?, reader.u8()?);

        if width > MAX_PNG_INTEGER || height > MAX_PNG_INTEGER {
            return Err(DecodeError::corrupt("PNG dimensions exceed 2^31 - 1"));
        }
        if !color_type.allows_depth(depth) {
            return Err(DecodeError::corrupt(format!("PNG bit depth {depth} is invalid here")));
        }
        if compression != COMPRESSION_DEFLATE || filter_method != 0 || interlace > INTERLACE_ADAM7 {
            return Err(DecodeError::unsupported("PNG compression, filter or interlace method"));
        }
        DecodeError::check_size(width, height)?;
        Ok(Self { width, height, depth, color_type, interlaced: interlace == INTERLACE_ADAM7 })
    }

    fn bits_per_pixel(&self) -> usize {
        self.color_type.channels() * usize::from(self.depth)
    }

    fn row_bytes(&self, width: usize) -> usize {
        (width * self.bits_per_pixel()).div_ceil(8)
    }

    /// Size of each pass in pixels; a single full-size pass when not interlaced.
    fn passes(&self) -> Vec<(usize, usize, usize, usize, usize, usize)> {
        let (width, height) = (self.width as usize, self.height as usize);
        if !self.interlaced {
            return vec![(0, 0, 1, 1, width, height)];
        }
        ADAM7
            .iter()
            .map(|&(x, y, step_x, step_y)| {
                let span = |size: usize, start: usize, step: usize| {
                    size.saturating_sub(start).div_ceil(step)
                };
                (x, y, step_x, step_y, span(width, x, step_x), span(height, y, step_y))
            })
            .filter(|&(.., pass_width, pass_height)| pass_width > 0 && pass_height > 0)
            .collect()
    }
}

/// Everything besides pixels that the chunks contribute.
#[derive(Default)]
struct Ancillary {
    palette: Vec<[u8; 3]>,
    transparency: Option<Vec<u8>>,
    icc_profile: Option<Vec<u8>>,
}

/// Walks the chunks, checking CRCs, and returns the header, extras and IDAT data.
fn read_chunks(bytes: &[u8]) -> Result<(Header, Ancillary, Vec<u8>), DecodeError> {
    let body =
        bytes.strip_prefix(&SIGNATURE).ok_or_else(|| DecodeError::unsupported("not a PNG file"))?;
    let mut reader = ByteReader::new(body, Endian::Big);
    let mut header = None;
    let mut extras = Ancillary::default();
    let mut idat = Vec::new();

    while reader.remaining() > 0 {
        let length = reader.u32()?;
        if length > MAX_PNG_INTEGER {
            return Err(DecodeError::corrupt("PNG chunk length exceeds 2^31 - 1"));
        }
        let kind: [u8; 4] = reader.bytes(4)?.try_into().expect("slice has 4 bytes");
        let data = reader.bytes(length as usize)?;
        let crc_ok = reader.u32()? == crc32_parts(&[&kind, data]);
        let ancillary = kind[0] & ANCILLARY_BIT != 0;

        if !crc_ok {
            // A damaged ancillary chunk is dropped, as libpng does.
            if ancillary {
                continue;
            }
            let name = String::from_utf8_lossy(&kind);
            return Err(DecodeError::corrupt(format!("PNG {name} chunk checksum mismatch")));
        }
        if &kind != chunk::IHDR && header.is_none() {
            return Err(DecodeError::corrupt("PNG does not start with a header chunk"));
        }
        match &kind {
            chunk::IHDR if header.is_none() => header = Some(Header::parse(data)?),
            chunk::IHDR => return Err(DecodeError::corrupt("PNG has two header chunks")),
            chunk::PLTE => extras.palette = parse_palette(data)?,
            chunk::IDAT => idat.extend_from_slice(data),
            chunk::IEND => break,
            chunk::TRNS => extras.transparency = Some(data.to_vec()),
            chunk::ICCP => extras.icc_profile = Some(parse_icc(data)?),
            _ if ancillary => {}
            _ => {
                let name = String::from_utf8_lossy(&kind);
                return Err(DecodeError::unsupported(format!("PNG critical chunk {name}")));
            }
        }
    }

    let header = header.ok_or_else(|| DecodeError::corrupt("PNG has no header chunk"))?;
    if idat.is_empty() {
        return Err(DecodeError::corrupt("PNG has no image data"));
    }
    if header.color_type == ColorType::Palette && extras.palette.is_empty() {
        return Err(DecodeError::corrupt("palette PNG has no palette"));
    }
    Ok((header, extras, idat))
}

fn parse_palette(data: &[u8]) -> Result<Vec<[u8; 3]>, DecodeError> {
    if data.is_empty() || data.len() % 3 != 0 || data.len() > 256 * 3 {
        return Err(DecodeError::corrupt("PNG palette has an invalid length"));
    }
    Ok(data.chunks_exact(3).map(|entry| [entry[0], entry[1], entry[2]]).collect())
}

/// iCCP holds a profile name, a compression method and the zlib-compressed profile.
fn parse_icc(data: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let name_end = data
        .iter()
        .position(|&byte| byte == 0)
        .ok_or_else(|| DecodeError::corrupt("PNG iCCP chunk has no profile name"))?;
    match data.get(name_end + 1..) {
        Some([COMPRESSION_DEFLATE, profile @ ..]) => zlib_decompress(profile, MAX_ICC_BYTES),
        _ => Err(DecodeError::unsupported("PNG iCCP compression method")),
    }
}

/// Turns one unfiltered row of samples into straight-alpha RGBA pixels.
struct PixelReader<'a> {
    header: &'a Header,
    palette: Vec<Rgba>,
    /// The raw sample values (grey, or red, green, blue) that mark a pixel transparent.
    transparent_key: Option<[u16; 3]>,
    max_sample: f32,
}

impl<'a> PixelReader<'a> {
    fn new(header: &'a Header, extras: &Ancillary) -> Self {
        let transparency = extras.transparency.as_deref().unwrap_or_default();
        let sample = |index: usize| {
            transparency
                .get(2 * index..2 * index + 2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        };
        let transparent_key = match header.color_type {
            ColorType::Gray => sample(0).map(|gray| [gray; 3]),
            ColorType::Rgb => match (sample(0), sample(1), sample(2)) {
                (Some(red), Some(green), Some(blue)) => Some([red, green, blue]),
                _ => None,
            },
            _ => None,
        };
        let palette = extras
            .palette
            .iter()
            .enumerate()
            .map(|(index, color)| {
                let alpha = transparency.get(index).copied().unwrap_or(u8::MAX);
                let [red, green, blue, alpha] = [color[0], color[1], color[2], alpha];
                [red, green, blue, alpha].map(|value| f32::from(value) / f32::from(u8::MAX))
            })
            .collect();
        let max_sample = ((1u32 << header.depth) - 1) as f32;
        Self { header, palette, transparent_key, max_sample }
    }

    fn sample(&self, row: &[u8], index: usize) -> u16 {
        match self.header.depth {
            16 => u16::from_be_bytes([row[2 * index], row[2 * index + 1]]),
            8 => u16::from(row[index]),
            depth => {
                let bit = index * usize::from(depth);
                let shift = 8 - usize::from(depth) - bit % 8;
                u16::from(row[bit / 8] >> shift) & ((1 << depth) - 1)
            }
        }
    }

    fn pixel(&self, row: &[u8], x: usize) -> Result<Rgba, DecodeError> {
        let channels = self.header.color_type.channels();
        let raw = |channel: usize| self.sample(row, x * channels + channel);
        let value = |channel: usize| f32::from(raw(channel)) / self.max_sample;
        let keyed = |key: [u16; 3]| if self.transparent_key == Some(key) { 0.0 } else { 1.0 };

        Ok(match self.header.color_type {
            ColorType::Gray => {
                let gray = value(0);
                [gray, gray, gray, keyed([raw(0); 3])]
            }
            ColorType::Rgb => [value(0), value(1), value(2), keyed([raw(0), raw(1), raw(2)])],
            ColorType::Palette => *self
                .palette
                .get(usize::from(raw(0)))
                .ok_or_else(|| DecodeError::corrupt("PNG palette index out of range"))?,
            ColorType::GrayAlpha => {
                let gray = value(0);
                [gray, gray, gray, value(1)]
            }
            ColorType::Rgba => [value(0), value(1), value(2), value(3)],
        })
    }
}

pub fn decode(bytes: &[u8]) -> Result<Image, DecodeError> {
    let (header, extras, idat) = read_chunks(bytes)?;
    let passes = header.passes();

    let expected: usize = passes
        .iter()
        .map(|&(.., pass_width, pass_height)| pass_height * (1 + header.row_bytes(pass_width)))
        .sum();
    let mut raw = zlib_decompress(&idat, expected)?;
    if raw.len() != expected {
        return Err(DecodeError::corrupt("PNG image data is shorter than the image"));
    }
    // Allocated only now, so a tiny file claiming a huge size fails before this.
    let width = header.width as usize;
    let mut pixels = vec![[0.0; 4]; DecodeError::check_size(header.width, header.height)?];

    let reader = PixelReader::new(&header, &extras);
    let bpp = header.bits_per_pixel().div_ceil(8);
    let mut offset = 0;
    for (start_x, start_y, step_x, step_y, pass_width, pass_height) in passes {
        let stride = 1 + header.row_bytes(pass_width);
        let pass = &mut raw[offset..offset + stride * pass_height];
        offset += stride * pass_height;
        let zeros = vec![0u8; stride - 1];

        for y in 0..pass_height {
            let (above, rest) = pass.split_at_mut(y * stride);
            let previous = if y == 0 { &zeros[..] } else { &above[above.len() - (stride - 1)..] };
            let (filter_type, row) =
                rest[..stride].split_first_mut().expect("stride is at least 1");
            filter::unfilter(*filter_type, row, previous, bpp)?;

            let target_row = (start_y + y * step_y) * width;
            for x in 0..pass_width {
                pixels[target_row + start_x + x * step_x] = reader.pixel(row, x)?;
            }
        }
    }

    let color_space = match extras.icc_profile {
        Some(profile) => ColorSpace::Icc(Arc::from(profile)),
        // sRGB, gAMA/cHRM-only and untagged files are all treated as sRGB.
        None => ColorSpace::Srgb,
    };
    Ok(Image::new(header.width, header.height, pixels, color_space)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::write_chunk;
    use mirante_codec::deflate::zlib_compress;

    /// Builds a PNG from header fields, extra chunks before IDAT and filtered rows.
    fn png(
        size: (u32, u32),
        depth: u8,
        color: u8,
        interlace: u8,
        extras: &[(&[u8; 4], &[u8])],
        rows: &[u8],
    ) -> Vec<u8> {
        let mut header = Vec::new();
        header.extend_from_slice(&size.0.to_be_bytes());
        header.extend_from_slice(&size.1.to_be_bytes());
        header.extend_from_slice(&[depth, color, 0, 0, interlace]);
        let mut file = SIGNATURE.to_vec();
        write_chunk(&mut file, chunk::IHDR, &header);
        for (kind, data) in extras {
            write_chunk(&mut file, kind, data);
        }
        // Two IDAT chunks check that the stream is joined.
        let compressed = zlib_compress(rows);
        let (first, second) = compressed.split_at(compressed.len() / 2);
        write_chunk(&mut file, chunk::IDAT, first);
        write_chunk(&mut file, chunk::IDAT, second);
        write_chunk(&mut file, chunk::IEND, &[]);
        file
    }

    #[test]
    fn decodes_one_bit_gray() {
        // Rows 10110 and 01001, padded to a byte, unfiltered.
        let file = png((5, 2), 1, 0, 0, &[], &[0, 0b1011_0000, 0, 0b0100_1000]);
        let image = decode(&file).unwrap();
        let grays: Vec<f32> = image.pixels().iter().map(|pixel| pixel[0]).collect();
        assert_eq!(grays, [1., 0., 1., 1., 0., 0., 1., 0., 0., 1.]);
        assert_eq!(image.color_space(), &ColorSpace::Srgb);
    }

    #[test]
    fn decodes_palette_with_transparency() {
        let palette: &[u8] = &[255, 0, 0, 0, 255, 0, 0, 0, 255];
        let alpha: &[u8] = &[0, 128];
        // Two-bit indices 0, 1, 2 with the Sub filter: deltas 0, 1, 1 within one byte.
        let file = png(
            (3, 1),
            2,
            3,
            0,
            &[(chunk::PLTE, palette), (chunk::TRNS, alpha)],
            &[0, 0b0001_1000],
        );
        let pixels = decode(&file).unwrap().pixels().to_vec();
        assert_eq!(pixels[0], [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(pixels[1], [0.0, 1.0, 0.0, 128.0 / 255.0]);
        assert_eq!(pixels[2], [0.0, 0.0, 1.0, 1.0]);

        let out_of_range = png((1, 1), 8, 3, 0, &[(chunk::PLTE, palette)], &[0, 3]);
        assert!(decode(&out_of_range).is_err());
    }

    #[test]
    fn decodes_rgb_with_colour_key_and_filters() {
        let key: &[u8] = &[0, 10, 0, 20, 0, 30];
        // Row 1 raw: (10,20,30) (40,50,60); row 2 uses Up: same row again.
        let rows = [1, 10, 20, 30, 30, 30, 30, 2, 0, 0, 0, 0, 0, 0];
        let pixels =
            decode(&png((2, 2), 8, 2, 0, &[(chunk::TRNS, key)], &rows)).unwrap().pixels().to_vec();
        assert_eq!(pixels[0][3], 0.0);
        assert_eq!(pixels[3], [40.0, 50.0, 60.0, 255.0].map(|value| value / 255.0));
        assert_eq!(pixels[2], pixels[0]);
    }

    #[test]
    fn decodes_sixteen_bit_gray_alpha() {
        let rows = [0, 0xFF, 0xFF, 0x80, 0x00];
        let pixel = decode(&png((1, 1), 16, 4, 0, &[], &rows)).unwrap().pixels()[0];
        assert_eq!(pixel, [1.0, 1.0, 1.0, 32768.0 / 65535.0]);
    }

    #[test]
    fn decodes_adam7_like_the_progressive_image() {
        let (width, height) = (11usize, 9usize);
        let value = |x: usize, y: usize| (x * 23 + y * 7) as u8;
        let mut progressive = Vec::new();
        for y in 0..height {
            progressive.push(0);
            progressive.extend((0..width).map(|x| value(x, y)));
        }
        let mut interlaced = Vec::new();
        for (start_x, start_y, step_x, step_y) in ADAM7 {
            for y in (start_y..height).step_by(step_y) {
                if start_x < width {
                    interlaced.push(0);
                    interlaced.extend((start_x..width).step_by(step_x).map(|x| value(x, y)));
                }
            }
        }
        let size = (width as u32, height as u32);
        let expected = decode(&png(size, 8, 0, 0, &[], &progressive)).unwrap();
        let actual = decode(&png(size, 8, 0, 1, &[], &interlaced)).unwrap();
        assert_eq!(actual.pixels(), expected.pixels());
    }

    #[test]
    fn reads_icc_profile() {
        let profile = b"not really a profile";
        let mut iccp = b"test\0\0".to_vec();
        iccp.extend_from_slice(&zlib_compress(profile));
        let image = decode(&png((1, 1), 8, 0, 0, &[(chunk::ICCP, &iccp)], &[0, 0])).unwrap();
        assert_eq!(image.color_space(), &ColorSpace::Icc(Arc::from(&profile[..])));
    }

    #[test]
    fn rejects_damaged_files() {
        let good = png((1, 1), 8, 0, 0, &[], &[0, 7]);
        assert!(decode(&good).is_ok());

        // The first chunk after IHDR starts at byte 33; its data starts at 41.
        let mut bad_crc = good.clone();
        bad_crc[41] ^= 1;
        assert!(decode(&bad_crc).is_err());
        assert!(decode(&good[..good.len() - 20]).is_err());
        assert!(decode(&png((2, 1), 8, 0, 0, &[], &[0, 7])).is_err());
        assert!(decode(&png((1, 1), 3, 0, 0, &[], &[0, 7])).is_err());
        assert!(decode(&png((1, 1), 8, 0, 0, &[], &[9, 7])).is_err());

        // A damaged ancillary chunk is skipped.
        let mut text = png((1, 1), 8, 0, 0, &[(b"tEXt", b"a\0b")], &[0, 7]);
        text[33 + 8 + 3] ^= 1;
        assert!(decode(&text).is_ok());
    }
}
