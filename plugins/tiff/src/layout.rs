//! Module: mirante_format_tiff::layout
//! Purpose: Reads and validates the IFD fields that describe how pixels are stored.
//! Created: 2026-11-16
//! Architecture: Everything the decoder supports is checked here once, so the
//! strip, sample and pixel stages can assume a consistent description.

use crate::samples::{PREDICTOR_FLOAT, PREDICTOR_HORIZONTAL, PREDICTOR_NONE};
use crate::samples::{RowFormat, SampleFormat};
use mirante_codec::tiff::{Ifd, Tiff, tag};
use mirante_core::DecodeError;

pub const FILL_ORDER: u16 = 266;
pub const INK_SET: u16 = 332;
pub const JPEG_TABLES: u16 = 347;

pub mod compression {
    pub const NONE: u16 = 1;
    pub const LZW: u16 = 5;
    pub const JPEG: u16 = 7;
    pub const DEFLATE: u16 = 8;
    pub const PACKBITS: u16 = 32773;
    pub const DEFLATE_OLD: u16 = 32946;
    pub const SUPPORTED: [u16; 6] = [NONE, LZW, JPEG, DEFLATE, PACKBITS, DEFLATE_OLD];
}

pub mod photometric {
    pub const WHITE_IS_ZERO: u16 = 0;
    pub const BLACK_IS_ZERO: u16 = 1;
    pub const RGB: u16 = 2;
    pub const PALETTE: u16 = 3;
    pub const SEPARATED: u16 = 5;
    pub const YCBCR: u16 = 6;
}

const SAMPLE_FORMAT_UNSIGNED: u32 = 1;
const SAMPLE_FORMAT_FLOAT: u32 = 3;
pub const PLANAR_CHUNKY: u16 = 1;
const PLANAR_SEPARATE: u32 = 2;
const INK_SET_CMYK: u32 = 1;
const EXTRA_ASSOCIATED_ALPHA: u16 = 1;
pub const EXTRA_UNASSOCIATED_ALPHA: u16 = 2;
const FILL_ORDER_LSB_FIRST: u32 = 2;
/// NewSubfileType bit marking a thumbnail or other reduced-resolution copy.
const REDUCED_RESOLUTION: u32 = 1;

#[derive(Clone, Copy, Debug)]
pub struct Alpha {
    pub sample: usize,
    pub associated: bool,
}

#[derive(Debug)]
pub struct Layout {
    pub width: u32,
    pub height: u32,
    pub samples_per_pixel: usize,
    /// 1 for grey and palette, 3 for RGB and YCbCr, 4 for CMYK.
    pub color_channels: usize,
    pub photometric: u16,
    pub compression: u16,
    pub separate_planes: bool,
    pub reversed_bits: bool,
    pub alpha: Option<Alpha>,
    pub row: RowFormat,
}

/// The first IFD of the main chain that is not a reduced-resolution copy.
pub fn full_resolution_ifd<'t>(tiff: &'t Tiff) -> Result<&'t Ifd, DecodeError> {
    for ifd in &tiff.ifds {
        let kind = tiff.get_u32(ifd, tag::NEW_SUBFILE_TYPE)?.unwrap_or(0);
        if kind & REDUCED_RESOLUTION == 0 {
            return Ok(ifd);
        }
    }
    Err(DecodeError::corrupt("TIFF has no full-resolution image"))
}

impl Layout {
    pub fn read(tiff: &Tiff, ifd: &Ifd) -> Result<Self, DecodeError> {
        let required = |tag| {
            tiff.get_u32(ifd, tag)?
                .ok_or_else(|| DecodeError::corrupt(format!("TIFF tag {tag} is missing")))
        };
        let optional =
            |tag, default| Ok::<_, DecodeError>(tiff.get_u32(ifd, tag)?.unwrap_or(default));
        let (width, height) = (required(tag::IMAGE_WIDTH)?, required(tag::IMAGE_LENGTH)?);
        DecodeError::check_size(width, height)?;
        let samples_per_pixel = optional(tag::SAMPLES_PER_PIXEL, 1)? as usize;
        let bits = uniform(tiff, ifd, tag::BITS_PER_SAMPLE, 1)?;
        let format = match uniform(tiff, ifd, tag::SAMPLE_FORMAT, SAMPLE_FORMAT_UNSIGNED)? {
            SAMPLE_FORMAT_UNSIGNED if matches!(bits, 1 | 2 | 4 | 8 | 16) => SampleFormat::Unsigned,
            SAMPLE_FORMAT_FLOAT if matches!(bits, 16 | 32) => SampleFormat::Float,
            other => {
                return Err(DecodeError::unsupported(format!(
                    "{bits}-bit samples of format {other}"
                )));
            }
        };
        let predictor = optional(tag::PREDICTOR, u32::from(PREDICTOR_NONE))? as u16;
        let predictor_fits = match predictor {
            PREDICTOR_NONE => true,
            PREDICTOR_HORIZONTAL => bits >= 8,
            PREDICTOR_FLOAT => format == SampleFormat::Float,
            _ => false,
        };
        if !predictor_fits {
            return Err(DecodeError::unsupported(format!(
                "predictor {predictor} with {bits}-bit samples"
            )));
        }
        let compression = optional(tag::COMPRESSION, u32::from(compression::NONE))? as u16;
        if !compression::SUPPORTED.contains(&compression) {
            return Err(DecodeError::unsupported(format!("TIFF compression {compression}")));
        }
        let photometric = required(tag::PHOTOMETRIC)? as u16;
        let color_channels = color_channels(tiff, ifd, photometric, compression, format)?;
        if samples_per_pixel < color_channels {
            return Err(DecodeError::corrupt("TIFF has fewer samples than colour channels"));
        }
        let separate_planes =
            optional(tag::PLANAR_CONFIGURATION, u32::from(PLANAR_CHUNKY))? == PLANAR_SEPARATE;
        let alpha = alpha(tiff, ifd, color_channels)?.filter(|a| a.sample < samples_per_pixel);
        let jpeg_fits = !separate_planes && bits == 8 && samples_per_pixel == color_channels;
        if compression == compression::JPEG && !jpeg_fits {
            return Err(DecodeError::unsupported("JPEG-compressed TIFF with this sample layout"));
        }
        let stride = if separate_planes { 1 } else { samples_per_pixel };
        Ok(Self {
            width,
            height,
            samples_per_pixel,
            color_channels,
            photometric,
            compression,
            separate_planes,
            reversed_bits: optional(FILL_ORDER, 1)? == FILL_ORDER_LSB_FIRST,
            alpha,
            row: RowFormat { bits, format, predictor, endian: tiff.endian(), stride },
        })
    }

    /// The colour map (all reds, then greens, then blues) of a palette image, 0..1.
    pub fn color_map(&self, tiff: &Tiff, ifd: &Ifd) -> Result<Vec<f32>, DecodeError> {
        let entry = ifd.get(tag::COLOR_MAP).ok_or_else(|| DecodeError::corrupt("no colour map"))?;
        let map = tiff.unsigned(entry)?;
        if map.len() < 3 << self.row.bits {
            return Err(DecodeError::corrupt("TIFF colour map is too short"));
        }
        // Like libtiff, recognise writers that store 8-bit colours unscaled or
        // shifted left by 8, which would otherwise never reach full intensity.
        let divisor = if map.iter().all(|&v| v <= 0xFF) {
            255.0
        } else if map.iter().all(|&v| v & 0xFF == 0) {
            65280.0
        } else {
            65535.0
        };
        Ok(map.iter().map(|&v| (v as f32 / divisor).min(1.0)).collect())
    }
}

fn color_channels(
    tiff: &Tiff,
    ifd: &Ifd,
    photometric: u16,
    compression: u16,
    format: SampleFormat,
) -> Result<usize, DecodeError> {
    use photometric::*;
    match photometric {
        WHITE_IS_ZERO | BLACK_IS_ZERO => Ok(1),
        RGB => Ok(3),
        PALETTE if format == SampleFormat::Unsigned => Ok(1),
        SEPARATED if tiff.get_u32(ifd, INK_SET)?.unwrap_or(INK_SET_CMYK) == INK_SET_CMYK => Ok(4),
        YCBCR if compression == compression::JPEG => Ok(3),
        YCBCR => Err(DecodeError::unsupported("YCbCr TIFF without JPEG compression")),
        _ => {
            Err(DecodeError::unsupported(format!("TIFF photometric interpretation {photometric}")))
        }
    }
}

/// The first extra sample marked as alpha; unspecified extra samples are ignored.
fn alpha(tiff: &Tiff, ifd: &Ifd, color_channels: usize) -> Result<Option<Alpha>, DecodeError> {
    let Some(entry) = ifd.get(tag::EXTRA_SAMPLES) else {
        return Ok(None);
    };
    let kinds: Vec<u16> = tiff.unsigned(entry)?.into_iter().map(|kind| kind as u16).collect();
    Ok(kinds
        .iter()
        .position(|&kind| matches!(kind, EXTRA_ASSOCIATED_ALPHA | EXTRA_UNASSOCIATED_ALPHA))
        .map(|index| Alpha {
            sample: color_channels + index,
            associated: kinds[index] == EXTRA_ASSOCIATED_ALPHA,
        }))
}

/// A per-sample field whose values must all agree, as this decoder requires.
fn uniform(tiff: &Tiff, ifd: &Ifd, tag: u16, default: u32) -> Result<u32, DecodeError> {
    let Some(entry) = ifd.get(tag) else {
        return Ok(default);
    };
    let values = tiff.unsigned(entry)?;
    match values.first() {
        Some(&first) if values.iter().all(|&v| v == first) => Ok(first),
        Some(_) => Err(DecodeError::unsupported(format!("TIFF tag {tag} differs between samples"))),
        None => Ok(default),
    }
}
