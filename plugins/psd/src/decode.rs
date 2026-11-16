//! Module: mirante_format_psd::decode
//! Purpose: Decodes the merged composite of a PSD or PSB file into an Image.
//! Created: 2026-11-16
//! Architecture: Layers are skipped; sections.rs supplies the header and hints.
//! 32-bit documents are linear light and are tagged linear sRGB, since their ICC
//! profile describes the gamma-encoded document. CMYK uses the naive model.

use crate::sections::{Header, Resources, layer_count, section};
use mirante_codec::color_models::cmyk_to_rgb;
use mirante_codec::{ByteReader, Endian, packbits};
use mirante_core::{ColorSpace, DecodeError, Image, Rgba};
use std::borrow::Cow;

mod mode {
    pub const BITMAP: u16 = 0;
    pub const GRAYSCALE: u16 = 1;
    pub const INDEXED: u16 = 2;
    pub const RGB: u16 = 3;
    pub const CMYK: u16 = 4;
    pub const DUOTONE: u16 = 8;
}

const COMPRESSION_RAW: u16 = 0;
const COMPRESSION_RLE: u16 = 1;
const PALETTE_BYTES: usize = 768;
const PALETTE_SIZE: usize = 256;

pub fn decode(bytes: &[u8]) -> Result<Image, DecodeError> {
    let mut reader = ByteReader::new(bytes, Endian::Big);
    let header = Header::read(&mut reader)?;
    let color_data = section(&mut reader)?;
    let resources = Resources::read(section(&mut reader)?)?;
    let layer_section_length = header.read_length(&mut reader)?;
    let layers = reader.bytes(layer_section_length)?;

    if !resources.has_merged_data {
        return Err(DecodeError::unsupported("PSD saved without a merged composite image"));
    }
    let (color_channels, depth_fits) = match header.mode {
        mode::BITMAP => (1, header.depth == 1),
        mode::INDEXED => (1, header.depth == 8),
        mode::GRAYSCALE | mode::DUOTONE => (1, matches!(header.depth, 8 | 16 | 32)),
        mode::RGB => (3, matches!(header.depth, 8 | 16 | 32)),
        mode::CMYK => (4, matches!(header.depth, 8 | 16 | 32)),
        other => return Err(DecodeError::unsupported(format!("Photoshop colour mode {other}"))),
    };
    if !depth_fits || header.channels < color_channels {
        return Err(DecodeError::unsupported(format!(
            "Photoshop colour mode {} at {} bits with {} channels",
            header.mode, header.depth, header.channels
        )));
    }
    // Photoshop marks merged transparency with a negative layer count; files
    // without layers carry it only as an unnamed extra channel.
    let has_alpha = header.channels > color_channels
        && match layer_count(layers, &header)? {
            Some(count) => count < 0,
            None => !resources.named_alpha_channels,
        };
    let planes = read_planes(&mut reader, &header, color_channels + usize::from(has_alpha))?;
    let palette = match header.mode {
        mode::INDEXED => Some(
            color_data.get(..PALETTE_BYTES).ok_or_else(|| DecodeError::corrupt("PSD palette"))?,
        ),
        _ => None,
    };
    let pixels = compose(&planes, &header, palette, resources.transparency_index, has_alpha);

    let color_space = match resources.icc {
        _ if header.depth == 32 => ColorSpace::LinearSrgb,
        Some(icc) if matches!(header.mode, mode::GRAYSCALE | mode::RGB) => {
            ColorSpace::Icc(icc.into())
        }
        _ => ColorSpace::Srgb,
    };
    Ok(Image::new(header.width, header.height, pixels, color_space)?)
}

/// The first `wanted` channel planes as samples: 0..1 for integers, the stored
/// value for floats, and the raw bit for 1-bit data.
fn read_planes(
    reader: &mut ByteReader,
    header: &Header,
    wanted: usize,
) -> Result<Vec<Vec<f32>>, DecodeError> {
    let compression = reader.u16()?;
    let height = header.height as usize;
    let row_bytes = (header.width as usize * usize::from(header.depth)).div_ceil(8);
    let count_bytes = if header.is_psb() { 4 } else { 2 };
    let row_counts = match compression {
        COMPRESSION_RAW => None,
        COMPRESSION_RLE => Some(reader.bytes(header.channels * height * count_bytes)?),
        other => {
            return Err(DecodeError::unsupported(format!("PSD composite compression {other}")));
        }
    };
    let mut planes = Vec::with_capacity(wanted);

    for channel in 0..wanted {
        let mut plane = Vec::with_capacity(header.width as usize * height);
        for row in 0..height {
            let stored = match row_counts {
                None => Cow::Borrowed(reader.bytes(row_bytes)?),
                Some(counts) => {
                    let at = (channel * height + row) * count_bytes;
                    let length = counts[at..at + count_bytes]
                        .iter()
                        .fold(0, |length, &byte| length << 8 | usize::from(byte));
                    Cow::Owned(packbits::unpack(reader.bytes(length)?, row_bytes)?)
                }
            };
            push_samples(&stored, header, &mut plane);
        }
        planes.push(plane);
    }
    Ok(planes)
}

fn push_samples(row: &[u8], header: &Header, plane: &mut Vec<f32>) {
    let width = header.width as usize;
    match header.depth {
        1 => plane.extend((0..width).map(|x| f32::from((row[x / 8] >> (7 - x % 8)) & 1))),
        8 => plane.extend(row.iter().map(|&v| f32::from(v) / 255.0)),
        16 => plane.extend(
            row.chunks_exact(2).map(|v| f32::from(u16::from_be_bytes([v[0], v[1]])) / 65535.0),
        ),
        _ => {
            plane.extend(row.chunks_exact(4).map(|v| f32::from_be_bytes([v[0], v[1], v[2], v[3]])))
        }
    }
}

fn compose(
    planes: &[Vec<f32>],
    header: &Header,
    palette: Option<&[u8]>,
    transparency_index: Option<u16>,
    has_alpha: bool,
) -> Vec<Rgba> {
    let channels = planes.len() - usize::from(has_alpha);
    let floats = header.depth == 32;

    (0..planes[0].len())
        .map(|index| {
            let sample = |channel: usize| planes[channel][index];
            let alpha = if has_alpha { sample(channels).clamp(0.0, 1.0) } else { 1.0 };
            let rgb = match header.mode {
                mode::BITMAP => [1.0 - sample(0); 3],
                mode::INDEXED => {
                    let entry = (sample(0) * 255.0).round() as usize;
                    let palette = palette.expect("indexed files carry a checked palette");
                    if transparency_index == Some(entry as u16) {
                        return [0.0; 4];
                    }
                    [0, 1, 2].map(|plane| f32::from(palette[plane * PALETTE_SIZE + entry]) / 255.0)
                }
                // CMYK is stored inverted: 0 means full ink.
                mode::CMYK => cmyk_to_rgb([0, 1, 2, 3].map(|channel| 1.0 - sample(channel))),
                mode::RGB => [sample(0), sample(1), sample(2)],
                _ => [sample(0); 3],
            };
            let [r, g, b] = if has_alpha { remove_white_matte(rgb, alpha, floats) } else { rgb };
            [r, g, b, alpha]
        })
        .collect()
}

/// The merged image is composited over white; undo that to get straight colour.
fn remove_white_matte(rgb: [f32; 3], alpha: f32, floats: bool) -> [f32; 3] {
    if alpha <= 0.0 {
        return rgb;
    }
    rgb.map(|value| {
        let straight = (value - (1.0 - alpha)) / alpha;
        if floats { straight.max(0.0) } else { straight.clamp(0.0, 1.0) }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x1 PSD; `layers` is the layer section body, `image` the image data.
    fn psd(mode: u16, channels: u16, resources: &[u8], layers: &[u8], image: &[u8]) -> Vec<u8> {
        let mut file = b"8BPS\0\x01\0\0\0\0\0\0".to_vec();
        file.extend(channels.to_be_bytes());
        file.extend(1_u32.to_be_bytes());
        file.extend(2_u32.to_be_bytes());
        file.extend(8_u16.to_be_bytes());
        file.extend(mode.to_be_bytes());
        file.extend(0_u32.to_be_bytes());
        for section in [resources, layers] {
            file.extend((section.len() as u32).to_be_bytes());
            file.extend_from_slice(section);
        }
        file.extend_from_slice(image);
        file
    }

    fn resource(id: u16, data: &[u8]) -> Vec<u8> {
        let mut block = b"8BIM".to_vec();
        block.extend(id.to_be_bytes());
        block.extend([0, 0]);
        block.extend((data.len() as u32).to_be_bytes());
        block.extend_from_slice(data);
        block.resize(block.len() + data.len() % 2, 0);
        block
    }

    #[test]
    fn decodes_rle_rgb_with_icc_profile() {
        // Three channel rows, each the run "two bytes of v" (0xFF, v).
        let mut image = vec![0, 1, 0, 2, 0, 2, 0, 2];
        image.extend([0xFF, 255, 0xFF, 0, 0xFF, 51]);
        let file = psd(mode::RGB, 3, &resource(0x040F, b"icc"), &[], &image);
        let decoded = decode(&file).unwrap();
        assert_eq!(decoded.pixels(), [[1.0, 0.0, 0.2, 1.0]; 2]);
        assert!(matches!(decoded.color_space(), ColorSpace::Icc(icc) if &icc[..] == b"icc"));
    }

    #[test]
    fn negative_layer_count_means_merged_alpha_over_white() {
        // Grey 0.6 at alpha 0.5 over white is stored as 0.8 (204).
        let layers = [0, 0, 0, 2, 0xFF, 0xFF];
        let image = [0, 0, 204, 0, 128, 255];
        let decoded = decode(&psd(mode::GRAYSCALE, 2, &[], &layers, &image)).unwrap();
        let [grey, _, _, alpha] = decoded.pixels()[0];
        assert!((grey - 0.6).abs() < 0.01 && (alpha - 128.0 / 255.0).abs() < 1e-6);
        assert_eq!(decoded.pixels()[1], [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn extra_channels_are_ignored_with_positive_layer_count() {
        let layers = [0, 0, 0, 2, 0, 1];
        let image = [0, 0, 255, 255, 0, 0];
        let decoded = decode(&psd(mode::GRAYSCALE, 2, &[], &layers, &image)).unwrap();
        assert_eq!(decoded.pixels(), [[1.0; 4]; 2]);
    }

    #[test]
    fn rejects_files_without_merged_data() {
        let version = resource(0x0421, &[0, 0, 0, 1, 0]);
        let file = psd(mode::RGB, 3, &version, &[], &[0, 0, 1, 2, 3, 4, 5, 6]);
        assert!(matches!(decode(&file), Err(DecodeError::Unsupported(_))));
    }
}
