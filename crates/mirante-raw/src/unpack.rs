//! Module: mirante_format_raw::unpack
//! Purpose: Reads a DNG raw IFD's strips or tiles into one plane of integer samples.
//! Created: 2026-11-16
//! Architecture: Handles uncompressed data at any bit depth up to 16 and lossless
//! JPEG (through mirante_format_jpeg::decode_lossless). Lossless frames are read
//! as a flat row-major stream, which also covers tiles whose JPEG components are
//! interleaved columns.

use mirante_codec::Endian;
use mirante_codec::tiff::{Ifd, Tiff, tag};
use mirante_core::DecodeError;

const UNCOMPRESSED: u32 = 1;
const LOSSLESS_JPEG: u32 = 7;
const DEFLATE: u32 = 8;
const MAX_BITS: u32 = 16;
const CHUNKY: u32 = 1;

/// Raw samples, `samples_per_pixel` interleaved per pixel, row by row.
#[derive(Clone, Debug)]
pub struct RawPlane {
    pub width: usize,
    pub height: usize,
    pub samples_per_pixel: usize,
    pub bits_per_sample: u32,
    pub samples: Vec<u16>,
}

/// One strip or tile: where it sits in the image and where its bytes are.
struct Segment<'a> {
    left: usize,
    top: usize,
    /// Coded size; tiles at the right and bottom edges extend past the image.
    width: usize,
    height: usize,
    bytes: &'a [u8],
}

fn required(tiff: &Tiff<'_>, ifd: &Ifd, tag: u16) -> Result<u32, DecodeError> {
    tiff.get_u32(ifd, tag)?
        .ok_or_else(|| DecodeError::corrupt(format!("DNG raw IFD lacks tag {tag}")))
}

pub fn read(tiff: &Tiff<'_>, ifd: &Ifd) -> Result<RawPlane, DecodeError> {
    let width = required(tiff, ifd, tag::IMAGE_WIDTH)?;
    let height = required(tiff, ifd, tag::IMAGE_LENGTH)?;
    let pixels = DecodeError::check_size(width, height)?;
    let samples_per_pixel = tiff.get_u32(ifd, tag::SAMPLES_PER_PIXEL)?.unwrap_or(1) as usize;
    let bits = tiff.get_u32(ifd, tag::BITS_PER_SAMPLE)?.unwrap_or(1);
    let compression = tiff.get_u32(ifd, tag::COMPRESSION)?.unwrap_or(UNCOMPRESSED);
    if !(1..=4).contains(&samples_per_pixel) || !(1..=MAX_BITS).contains(&bits) {
        return Err(DecodeError::unsupported(format!(
            "DNG with {samples_per_pixel}x{bits}-bit samples"
        )));
    }
    if tiff.get_u32(ifd, tag::PLANAR_CONFIGURATION)?.unwrap_or(CHUNKY) != CHUNKY {
        return Err(DecodeError::unsupported("planar DNG raw data"));
    }
    if !matches!(compression, UNCOMPRESSED | LOSSLESS_JPEG) {
        let name =
            if compression == DEFLATE { "deflate".to_owned() } else { compression.to_string() };
        return Err(DecodeError::unsupported(format!("DNG compression {name}")));
    }
    let segments = segments(tiff, ifd, width as usize, height as usize)?;
    let total = pixels * samples_per_pixel;
    // Uncompressed data must be all there before we allocate for it.
    let stored: usize = segments.iter().map(|segment| segment.bytes.len()).sum();
    if compression == UNCOMPRESSED && stored < (total * bits as usize).div_ceil(8) {
        return Err(DecodeError::corrupt("DNG raw data is truncated"));
    }
    let mut plane = RawPlane {
        width: width as usize,
        height: height as usize,
        samples_per_pixel,
        bits_per_sample: bits,
        samples: vec![0; total],
    };
    for segment in &segments {
        match compression {
            UNCOMPRESSED => unpack_bits(&mut plane, segment, tiff.endian())?,
            _ => unpack_lossless(&mut plane, segment)?,
        }
    }
    Ok(plane)
}

fn segments<'a>(
    tiff: &Tiff<'a>,
    ifd: &Ifd,
    width: usize,
    height: usize,
) -> Result<Vec<Segment<'a>>, DecodeError> {
    let tiled = ifd.get(tag::TILE_OFFSETS).is_some();
    let (offsets, counts) = if tiled {
        (tag::TILE_OFFSETS, tag::TILE_BYTE_COUNTS)
    } else {
        (tag::STRIP_OFFSETS, tag::STRIP_BYTE_COUNTS)
    };
    let list = |tag| ifd.get(tag).map(|entry| tiff.unsigned(entry)).transpose();
    let (Some(offsets), Some(counts)) = (list(offsets)?, list(counts)?) else {
        return Err(DecodeError::corrupt("DNG raw IFD has no data offsets"));
    };
    let (segment_width, segment_height) = if tiled {
        (
            required(tiff, ifd, tag::TILE_WIDTH)? as usize,
            required(tiff, ifd, tag::TILE_LENGTH)? as usize,
        )
    } else {
        let rows = tiff.get_u32(ifd, tag::ROWS_PER_STRIP)?.unwrap_or(u32::MAX) as usize;
        (width, rows.min(height))
    };
    if segment_width == 0 || segment_height == 0 {
        return Err(DecodeError::corrupt("DNG tile or strip has no size"));
    }
    let across = width.div_ceil(segment_width);
    let expected = across * height.div_ceil(segment_height);
    if offsets.len() != expected || counts.len() != expected {
        return Err(DecodeError::corrupt("DNG strip or tile count does not match the image"));
    }
    offsets
        .iter()
        .zip(&counts)
        .enumerate()
        .map(|(index, (&offset, &count))| {
            let bytes =
                tiff.data().get(offset as usize..).and_then(|rest| rest.get(..count as usize));
            let bytes =
                bytes.ok_or_else(|| DecodeError::corrupt("DNG raw data lies outside the file"))?;
            let (left, top) = ((index % across) * segment_width, (index / across) * segment_height);
            let height = if tiled { segment_height } else { segment_height.min(height - top) };
            Ok(Segment { left, top, width: segment_width, height, bytes })
        })
        .collect()
}

/// Copies one coded segment row into the plane, dropping columns past the edge.
fn place_row(
    plane: &mut RawPlane,
    segment: &Segment<'_>,
    row: usize,
    values: impl Iterator<Item = u16>,
) {
    let y = segment.top + row;
    if y >= plane.height {
        return;
    }
    let spp = plane.samples_per_pixel;
    let visible = segment.width.min(plane.width - segment.left) * spp;
    let start = (y * plane.width + segment.left) * spp;
    for (target, value) in plane.samples[start..start + visible].iter_mut().zip(values) {
        *target = value;
    }
}

/// Uncompressed rows start on byte boundaries; samples are packed most significant
/// bit first, except that 16-bit samples follow the file's byte order.
fn unpack_bits(
    plane: &mut RawPlane,
    segment: &Segment<'_>,
    endian: Endian,
) -> Result<(), DecodeError> {
    let row_samples = segment.width * plane.samples_per_pixel;
    let row_bytes = (row_samples * plane.bits_per_sample as usize).div_ceil(8);
    let rows = segment.height.min(plane.height.saturating_sub(segment.top));
    if segment.bytes.len() < row_bytes * rows {
        return Err(DecodeError::corrupt("DNG strip or tile is truncated"));
    }
    for (row, bytes) in segment.bytes.chunks_exact(row_bytes).take(rows).enumerate() {
        let bits = plane.bits_per_sample;
        match bits {
            8 => place_row(plane, segment, row, bytes.iter().map(|&b| u16::from(b))),
            16 => {
                let words = bytes.chunks_exact(2).map(|pair| match endian {
                    Endian::Little => u16::from_le_bytes([pair[0], pair[1]]),
                    Endian::Big => u16::from_be_bytes([pair[0], pair[1]]),
                });
                place_row(plane, segment, row, words);
            }
            _ => place_row(plane, segment, row, PackedBits::new(bytes, bits).take(row_samples)),
        }
    }
    Ok(())
}

fn unpack_lossless(plane: &mut RawPlane, segment: &Segment<'_>) -> Result<(), DecodeError> {
    let frame = mirante_format_jpeg::decode_lossless(segment.bytes)?;
    let row_samples = segment.width * plane.samples_per_pixel;
    let rows = segment.height.min(plane.height.saturating_sub(segment.top));
    if frame.samples.len() < row_samples * rows {
        return Err(DecodeError::corrupt("lossless JPEG tile is smaller than its DNG tile"));
    }
    for (row, values) in frame.samples.chunks_exact(row_samples).take(rows).enumerate() {
        place_row(plane, segment, row, values.iter().copied());
    }
    Ok(())
}

/// Reads MSB-first packed samples of `bits` (1..=16) bits each, as many
/// uncompressed RAW formats store them.
pub struct PackedBits<'a> {
    bytes: std::slice::Iter<'a, u8>,
    bits: u32,
    buffer: u32,
    buffered: u32,
}

impl<'a> PackedBits<'a> {
    pub fn new(bytes: &'a [u8], bits: u32) -> Self {
        let bits = bits.clamp(1, 16);
        Self { bytes: bytes.iter(), bits, buffer: 0, buffered: 0 }
    }
}

impl Iterator for PackedBits<'_> {
    type Item = u16;

    fn next(&mut self) -> Option<u16> {
        while self.buffered < self.bits {
            self.buffer = (self.buffer << 8) | u32::from(*self.bytes.next()?);
            self.buffered += 8;
        }
        self.buffered -= self.bits;
        Some(((self.buffer >> self.buffered) & ((1 << self.bits) - 1)) as u16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpacks_twelve_and_ten_bit_samples() {
        let twelve: Vec<u16> = PackedBits::new(&[0xAB, 0xCD, 0xEF], 12).collect();
        assert_eq!(twelve, [0xABC, 0xDEF]);
        let ten: Vec<u16> = PackedBits::new(&[0xFF, 0xC0, 0x0F, 0xF0, 0x00], 10).collect();
        assert_eq!(ten, [0x3FF, 0x000, 0x3FC, 0x000]);
    }
}
