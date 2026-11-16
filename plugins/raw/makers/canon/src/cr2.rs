//! Module: mirante_raw_canon::cr2
//! Purpose: Reads CR2 files: a TIFF whose fourth IFD holds the raw image as a
//! lossless JPEG cut into vertical slices (tag 0xC640), Bayer or sRAW/mRAW.
//! Created: 2026-11-16
//! Architecture: Bayer data goes through mirante_format_jpeg::decode_lossless
//! and is de-sliced here; sRAW needs ljpeg.rs and sraw.rs. Slice layout and
//! quirks ported from dcraw's lossless_jpeg_load_raw by Dave Coffin.

use crate::assemble::{self, Camera, MAKE};
use crate::ljpeg::{self, LosslessJpeg};
use crate::maker_note::MakerNote;
use crate::sraw;
use mirante_codec::tiff::{Ifd, Tiff};
use mirante_core::DecodeError;
use mirante_raw::demosaic::{BLUE, CfaPattern, GREEN, RED};
use mirante_raw::tiff_raw;
use mirante_raw::unpack::RawPlane;
use mirante_raw::{Area, BlackLevel, NativeRaw};

const RAW_IFD: usize = 3;
const SLICES: u16 = 0xC640;
const MAKER_NOTE: u16 = 0x927C;
/// dcraw shifts this sensor's data two columns left.
const SHIFTED_WIDTH: usize = 3984;
const SHIFT: usize = 2;
/// sRAW data is converted to the camera's 14-bit scale.
const SRAW_WHITE: f32 = 16383.0;
const SRAW_BITS: u32 = 16;

pub fn decode(bytes: &[u8]) -> Result<NativeRaw, DecodeError> {
    let tiff = Tiff::parse(bytes)?;
    let raw_ifd =
        tiff.ifds.get(RAW_IFD).ok_or_else(|| DecodeError::unsupported("CR2 without a raw IFD"))?;
    let strip = tiff_raw::strip_data(&tiff, raw_ifd)?;
    let slices = slice_layout(&tiff, raw_ifd)?;
    let camera = Camera {
        model: tiff_raw::model_name(&tiff, MAKE)?,
        orientation: tiff_raw::orientation(&tiff)?,
        note: maker_note(&tiff)?,
    };
    if ljpeg::is_subsampled(strip)? {
        return decode_sraw(strip, slices, camera);
    }
    let frame = mirante_format_jpeg::decode_lossless(strip)?;
    let (width, height) = raw_size(
        frame.width as usize,
        frame.height as usize,
        frame.components as usize,
        slices,
        frame.samples.len(),
    )?;
    let mut samples = match slices {
        Some(layout) => deslice(&frame.samples, width, height, layout)?,
        None => frame.samples[..width * height].to_vec(),
    };
    if width == SHIFTED_WIDTH {
        samples.copy_within(SHIFT.., 0);
        let end = samples.len();
        samples[end - SHIFT..].fill(0);
    }
    let plane = RawPlane {
        width,
        height,
        samples_per_pixel: 1,
        bits_per_sample: u32::from(frame.precision),
        samples,
    };
    Ok(assemble::bayer(plane, cfa_pattern(), camera))
}

/// Canon sensors start with a red-green row at the top-left photosite.
fn cfa_pattern() -> CfaPattern {
    CfaPattern::new(2, 2, vec![RED, GREEN, GREEN, BLUE]).expect("valid Bayer pattern")
}

fn slice_layout(tiff: &Tiff<'_>, ifd: &Ifd) -> Result<Option<[usize; 3]>, DecodeError> {
    let Some(entry) = ifd.get(SLICES) else {
        return Ok(None);
    };
    match tiff.unsigned(entry)?[..] {
        [count, width, last] if count > 0 && width > 0 && last > 0 => {
            Ok(Some([count as usize, width as usize, last as usize]))
        }
        [0, ..] => Ok(None),
        _ => Err(DecodeError::corrupt("bad CR2 slice tag")),
    }
}

/// The Canon maker note: an IFD inside the EXIF MakerNote, with offsets
/// relative to the start of the file.
fn maker_note(tiff: &Tiff<'_>) -> Result<MakerNote, DecodeError> {
    let exif = tiff_raw::exif_ifd(tiff)?;
    let Some(entry) = exif.as_ref().and_then(|exif| exif.get(MAKER_NOTE)) else {
        return Ok(MakerNote::default());
    };
    let note = tiff.raw_bytes(entry)?;
    let offset = note.as_ptr().addr() - tiff.data().as_ptr().addr();
    let (ifd, _) = tiff.read_ifd(offset)?;
    MakerNote::read(tiff, &ifd)
}

/// Raw size as dcraw derives it: the slices give the width; without them the
/// JPEG rows are the raw rows, halved in width when they hold two rows.
fn raw_size(
    jpeg_width: usize,
    jpeg_height: usize,
    components: usize,
    slices: Option<[usize; 3]>,
    total: usize,
) -> Result<(usize, usize), DecodeError> {
    let (width, height) = match slices {
        Some([count, width, last]) => {
            let width = count * width + last;
            (width, total / width)
        }
        None => {
            let width = jpeg_width * components;
            if width > 4 * jpeg_height && components % 2 == 0 {
                (width / 2, jpeg_height * 2)
            } else {
                (width, jpeg_height)
            }
        }
    };
    if width == 0 || height == 0 || width * height > total {
        return Err(DecodeError::corrupt("CR2 slices do not match the raw data"));
    }
    DecodeError::check_size(width as u32, height as u32)?;
    Ok((width, height))
}

/// The JPEG holds the image slice by slice: all rows of the first `width`
/// columns, then the next slice, and the last slice with its own width.
pub fn deslice(
    stream: &[u16],
    width: usize,
    height: usize,
    [count, slice_width, last_width]: [usize; 3],
) -> Result<Vec<u16>, DecodeError> {
    if count * slice_width + last_width != width || stream.len() < width * height {
        return Err(DecodeError::corrupt("CR2 slices do not match the raw data"));
    }
    let mut out = vec![0; width * height];
    let mut offset = 0;
    for slice in 0..=count {
        let columns = if slice < count { slice_width } else { last_width };
        let left = slice * slice_width;
        for row in out.chunks_exact_mut(width) {
            row[left..left + columns].copy_from_slice(&stream[offset..offset + columns]);
            offset += columns;
        }
    }
    Ok(out)
}

fn decode_sraw(
    strip: &[u8],
    slices: Option<[usize; 3]>,
    camera: Camera,
) -> Result<NativeRaw, DecodeError> {
    let jpeg = LosslessJpeg::parse(strip)?;
    let color = camera.note.color.clone().unwrap_or_default();
    let multipliers = color
        .sraw_mul
        .ok_or_else(|| DecodeError::unsupported("Canon sRAW without its ColorData multipliers"))?;
    let params = sraw::Camera {
        model_id: camera.note.model_id.unwrap_or(0),
        firmware: camera.note.firmware.as_deref(),
        // LibRaw scales the entry by its own green (1170), giving it a 1/1024 unit.
        multipliers: multipliers.map(|m| ((u32::from(m) * u32::from(multipliers[1])) >> 10) as u16),
    };
    let rgb = sraw::decode(&jpeg, slices, &params)?;
    Ok(NativeRaw {
        visible: Area { left: 0, top: 0, width: rgb.width, height: rgb.height },
        plane: RawPlane {
            width: rgb.width,
            height: rgb.height,
            samples_per_pixel: 3,
            bits_per_sample: SRAW_BITS,
            samples: rgb.samples,
        },
        pattern: None,
        black: BlackLevel::uniform(0.0),
        white: SRAW_WHITE,
        white_balance: color.as_shot.map(|[r, g, _, b]| [r, g, b].map(f64::from)),
        make: MAKE.to_owned(),
        model: camera.model,
        orientation: camera.orientation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reassembles_slices() {
        // A 5x2 image cut into slices of 2, 2 and 1 columns.
        let image: Vec<u16> = (0..10).collect();
        let mut stream = Vec::new();
        for (left, columns) in [(0, 2), (2, 2), (4, 1)] {
            for row in 0..2 {
                stream.extend_from_slice(&image[row * 5 + left..][..columns]);
            }
        }
        assert_eq!(deslice(&stream, 5, 2, [2, 2, 1]).unwrap(), image);
        assert!(deslice(&stream, 6, 2, [2, 2, 1]).is_err());
    }

    #[test]
    fn derives_the_raw_size() {
        assert_eq!(
            raw_size(1480, 3950, 4, Some([2, 1920, 2080]), 5920 * 3950).unwrap(),
            (5920, 3950)
        );
        assert_eq!(raw_size(2000, 400, 2, None, 4000 * 400).unwrap(), (2000, 800));
        assert!(raw_size(100, 10, 1, Some([1, 50, 60]), 100).is_err());
    }
}
