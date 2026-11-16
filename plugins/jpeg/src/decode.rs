//! Module: mirante_format_jpeg::decode
//! Purpose: Decodes JPEG files into an Image in the file's colour space.
//! Created: 2026-11-16
//! Architecture: Every scan, baseline or progressive, fills a per-component store
//! of quantized coefficients (blocks.rs); after the last scan the blocks go through
//! the IDCT (dct.rs) and colour conversion (output.rs). Lossless frames are handed
//! to lossless.rs. Missing data decodes as mid-grey.

use crate::blocks::BlockDecoder;
use crate::lossless::decode_lossless;
use crate::markers::{
    APP0, APP15, Coding, DHT, DQT, DRI, EOI, Frame, SOS, Scan, Segments, Tables, is_frame_marker,
    parse_frame, parse_scan,
};
use crate::metadata::Metadata;
use crate::output::{ColorModel, Plane, convert};
use crate::scan::{McuDecoder, ScanEnd, decode_scan};
use crate::{dct, huffman::BitReader};
use mirante_core::{ColorSpace, DecodeError, Image, Rgba};

const BLOCK_SIZE: usize = 8;
/// The only sample precision of the DCT processes we decode.
const PRECISION: u8 = 8;
const EMPTY_BLOCK: [i16; 64] = [0; 64];

pub fn decode(bytes: &[u8]) -> Result<Image, DecodeError> {
    let mut segments = Segments::new(bytes)?;
    let mut tables = Tables::default();
    let mut metadata = Metadata::default();
    let mut coefficients: Option<Coefficients> = None;
    let mut decoded_mcus = 0;

    while let Some(segment) = segments.next_segment()? {
        match segment.marker {
            EOI => break,
            DHT => tables.read_huffman(segment.payload)?,
            DQT => tables.read_quant(segment.payload)?,
            DRI => tables.read_restart(segment.payload)?,
            SOS => {
                let Some(store) = coefficients.as_mut() else {
                    return Err(DecodeError::corrupt("JPEG scan before the frame header"));
                };
                let scan = parse_scan(segment.payload, &store.frame)?;
                let end = store.decode_scan(&scan, &tables, bytes, segments.position())?;
                decoded_mcus += end.decoded_mcus;
                segments.seek(end.position);
            }
            marker if is_frame_marker(marker) => {
                if coefficients.is_some() {
                    break;
                }
                let frame = parse_frame(marker, segment.payload)?;
                if frame.coding == Coding::Lossless {
                    return lossless_image(bytes, &metadata);
                }
                if frame.precision != PRECISION {
                    let precision = frame.precision;
                    return Err(DecodeError::unsupported(format!("{precision}-bit JPEG")));
                }
                coefficients = Some(Coefficients::new(frame)?);
            }
            marker if (APP0..=APP15).contains(&marker) => metadata.read(marker, segment.payload),
            _ => {}
        }
    }
    let Some(store) = coefficients else {
        return Err(DecodeError::corrupt("JPEG has no frame header"));
    };
    if decoded_mcus == 0 {
        return Err(DecodeError::corrupt("JPEG has no decodable image data"));
    }
    let model = color_model(&store.frame, &metadata)?;
    let pixels = store.to_pixels(model, metadata.adobe_transform.is_some());
    finish(&metadata, model, (store.frame.width, store.frame.height), pixels)
}

fn color_model(frame: &Frame, metadata: &Metadata<'_>) -> Result<ColorModel, DecodeError> {
    let ids: Vec<u8> = frame.components.iter().map(|c| c.id).collect();
    Ok(match (ids.len(), metadata.adobe_transform) {
        (1, _) => ColorModel::Gray,
        (3, Some(0)) => ColorModel::Rgb,
        (3, None) if !metadata.jfif && ids == b"RGB" => ColorModel::Rgb,
        (3, _) => ColorModel::YCbCr,
        (4, Some(2)) => ColorModel::Ycck,
        (4, _) => ColorModel::Cmyk,
        (count, _) => {
            return Err(DecodeError::unsupported(format!("JPEG with {count} components")));
        }
    })
}

/// Wraps pixels with the embedded profile and turns them upright.
fn finish(
    metadata: &Metadata<'_>,
    model: ColorModel,
    (width, height): (u32, u32),
    pixels: Vec<Rgba>,
) -> Result<Image, DecodeError> {
    // CMYK profiles describe ink, not the naive RGB computed from it.
    let space = match metadata.icc_profile() {
        Some(profile) if !matches!(model, ColorModel::Cmyk | ColorModel::Ycck) => {
            ColorSpace::Icc(profile.into())
        }
        _ => ColorSpace::Srgb,
    };
    Ok(Image::new(width, height, pixels, space)?.oriented(metadata.orientation()))
}

/// Coefficient blocks of every component, filled scan by scan.
struct Coefficients {
    frame: Frame,
    components: Vec<ComponentBlocks>,
    mcus_x: usize,
    mcus_y: usize,
}

struct ComponentBlocks {
    blocks: Vec<[i16; 64]>,
    blocks_x: usize,
    /// Latched at the component's first scan, as libjpeg does.
    quant: Option<[u16; 64]>,
}

impl Coefficients {
    fn new(frame: Frame) -> Result<Self, DecodeError> {
        DecodeError::check_size(frame.width, frame.height)?;
        let mcus_x = (frame.width as usize).div_ceil(BLOCK_SIZE * frame.h_max);
        let mcus_y = (frame.height as usize).div_ceil(BLOCK_SIZE * frame.v_max);
        let components = frame
            .components
            .iter()
            .map(|c| ComponentBlocks {
                blocks: vec![EMPTY_BLOCK; mcus_x * c.h * mcus_y * c.v],
                blocks_x: mcus_x * c.h,
                quant: None,
            })
            .collect();
        Ok(Self { frame, components, mcus_x, mcus_y })
    }

    fn decode_scan(
        &mut self,
        scan: &Scan,
        tables: &Tables,
        data: &[u8],
        start: usize,
    ) -> Result<ScanEnd, DecodeError> {
        for component in &scan.components {
            let quant_slot = self.frame.components[component.index].quant_slot;
            let blocks = &mut self.components[component.index];
            if blocks.quant.is_none() {
                blocks.quant = Some(tables.quant(quant_slot)?);
            }
        }
        let progressive = self.frame.coding == Coding::Progressive;
        let decoder = BlockDecoder::new(scan, tables, progressive)?;
        // A single-component scan covers only that component's blocks, not whole MCUs.
        let (columns, mcu_count, layout) = if let [only] = scan.components[..] {
            let (width, height) = self.frame.component_size(only.index);
            let columns = width.div_ceil(BLOCK_SIZE);
            (columns, columns * height.div_ceil(BLOCK_SIZE), vec![(only.index, 1, 1)])
        } else {
            let layout = scan.components.iter().map(|c| {
                let component = &self.frame.components[c.index];
                (c.index, component.h, component.v)
            });
            (self.mcus_x, self.mcus_x * self.mcus_y, layout.collect())
        };
        let mut scan_state =
            ScanState { decoder, components: &mut self.components, layout, columns };
        Ok(decode_scan(data, start, tables.restart_interval, mcu_count, &mut scan_state))
    }

    fn to_pixels(&self, model: ColorModel, inverted: bool) -> Vec<Rgba> {
        let frame = &self.frame;
        let size = (frame.width as usize, frame.height as usize);
        let planes: Vec<Vec<u8>> = self
            .components
            .iter()
            .enumerate()
            .map(|(index, blocks)| {
                let (width, height) = frame.component_size(index);
                let component = &frame.components[index];
                let plane = blocks.to_plane(width, height);
                plane.upsample((component.h, component.v), (frame.h_max, frame.v_max), size)
            })
            .collect();
        convert(model, &planes, inverted)
    }
}

impl ComponentBlocks {
    fn to_plane(&self, width: usize, height: usize) -> Plane {
        let stride = self.blocks_x * BLOCK_SIZE;
        let rows = self.blocks.len() / self.blocks_x;
        let mut samples = vec![0; stride * rows * BLOCK_SIZE];
        // A component without any scan has no table; its zero blocks decode to grey anyway.
        let table = dct::inverse_table(&self.quant.unwrap_or([1; 64]));

        for (index, block) in self.blocks.iter().enumerate() {
            let (x, y) = (index % self.blocks_x, index / self.blocks_x);
            let offset = y * BLOCK_SIZE * stride + x * BLOCK_SIZE;
            dct::inverse(block, &table, &mut samples[offset..], stride);
        }
        Plane { samples, stride, width, height }
    }
}

/// Maps MCU numbers of one scan to blocks.
struct ScanState<'s, 't> {
    decoder: BlockDecoder<'t>,
    components: &'s mut [ComponentBlocks],
    /// (frame component index, blocks across, blocks down) per MCU, in scan order.
    layout: Vec<(usize, usize, usize)>,
    /// MCUs per row of the scan.
    columns: usize,
}

impl McuDecoder for ScanState<'_, '_> {
    fn decode_mcu(&mut self, reader: &mut BitReader<'_>, mcu: usize) -> Result<(), DecodeError> {
        let (mcu_x, mcu_y) = (mcu % self.columns, mcu / self.columns);

        for (scan_index, &(index, across, down)) in self.layout.iter().enumerate() {
            let component = &mut self.components[index];
            for row in 0..down {
                for column in 0..across {
                    let (x, y) = (mcu_x * across + column, mcu_y * down + row);
                    let block = &mut component.blocks[y * component.blocks_x + x];
                    self.decoder.decode(reader, scan_index, block)?;
                }
            }
        }
        Ok(())
    }

    fn restart(&mut self, _next_mcu: usize) {
        self.decoder.restart();
    }
}

/// A standalone lossless JPEG shown as gray or RGB.
fn lossless_image(bytes: &[u8], metadata: &Metadata<'_>) -> Result<Image, DecodeError> {
    let frame = decode_lossless(bytes)?;
    let scale = 1.0 / ((1_u32 << frame.precision) - 1) as f32;
    let value = |sample: u16| f32::from(sample) * scale;
    let pixel_count = frame.width as usize * frame.height as usize;
    let (model, pixels): (ColorModel, Vec<Rgba>) = match frame.components {
        1 => (
            ColorModel::Gray,
            frame.samples.iter().map(|&s| [value(s), value(s), value(s), 1.0]).collect(),
        ),
        3 => (
            ColorModel::Rgb,
            frame
                .samples
                .chunks_exact(3)
                .map(|p| [value(p[0]), value(p[1]), value(p[2]), 1.0])
                .collect(),
        ),
        count => {
            return Err(DecodeError::unsupported(format!("lossless JPEG with {count} components")));
        }
    };
    if pixels.len() != pixel_count {
        return Err(DecodeError::corrupt("lossless JPEG sample count mismatch"));
    }
    finish(metadata, model, (frame.width, frame.height), pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::{encode, encode_with_restarts};
    use crate::markers::{RST0, SOF0, SOI};

    const COLOURS: [[f32; 4]; 4] =
        [[1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0], [1.0, 1.0, 0.0, 1.0]];
    const GREY: f32 = 128.0 / 255.0;

    /// Four 16x16 MCUs side by side, one flat colour each.
    fn stripes() -> Image {
        let pixels = (0..64 * 16).map(|i| COLOURS[(i % 64) / 16]).collect();
        Image::new(64, 16, pixels, ColorSpace::Srgb).unwrap()
    }

    fn find(bytes: &[u8], marker: u8) -> usize {
        bytes.windows(2).position(|pair| pair == [0xFF, marker]).unwrap()
    }

    fn close(pixel: Rgba, expected: [f32; 3]) -> bool {
        (0..3).all(|c| (pixel[c] - expected[c]).abs() < 0.05)
    }

    #[test]
    fn resynchronises_after_a_lost_restart_interval() {
        let mut bytes = encode_with_restarts(&stripes(), 1).unwrap();
        // Drop the second interval and its RST1, as if a chunk of the file were lost.
        let (first, second) = (find(&bytes, RST0), find(&bytes, RST0 + 1));
        bytes.drain(first + 2..second + 2);
        let image = decode(&bytes).unwrap();
        let centre = |mcu: usize| image.pixels()[8 * 64 + mcu * 16 + 8];
        assert!(close(centre(0), [1.0, 0.0, 0.0]));
        assert!(close(centre(2), [GREY; 3]));
        assert!(close(centre(3), [1.0, 1.0, 0.0]));
    }

    #[test]
    fn truncated_files_decode_with_grey_remainder() {
        let pixels = (0..64 * 64).map(|i| [(i % 64) as f32 / 64.0, 0.3, 0.6, 1.0]).collect();
        let source = Image::new(64, 64, pixels, ColorSpace::Srgb).unwrap();
        let bytes = encode(&source).unwrap();
        let data_start = find(&bytes, SOS);
        let image = decode(&bytes[..(data_start + bytes.len()) / 2]).unwrap();
        assert!(close(image.pixels()[0], [0.0, 0.3, 0.6]));
        assert!(close(*image.pixels().last().unwrap(), [GREY; 3]));
    }

    #[test]
    fn applies_exif_orientation() {
        let bytes = encode(&stripes()).unwrap();
        let mut tiff = b"Exif\0\0II*\0\x08\0\0\0\x01\0".to_vec();
        tiff.extend([0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0]);
        let mut file = vec![0xFF, SOI];
        crate::encode::write_segment(&mut file, crate::markers::APP1, &tiff);
        file.extend_from_slice(&bytes[2..]);
        let image = decode(&file).unwrap();
        assert_eq!((image.width(), image.height()), (16, 64));
    }

    #[test]
    fn reports_arithmetic_coding_and_12_bit_as_unsupported() {
        let bytes = encode(&stripes()).unwrap();
        let frame = find(&bytes, SOF0);
        let mut arithmetic = bytes.clone();
        arithmetic[frame + 1] = 0xC9;
        let mut twelve_bit = bytes;
        twelve_bit[frame + 4] = 12;
        assert!(matches!(decode(&arithmetic), Err(DecodeError::Unsupported(_))));
        assert!(matches!(decode(&twelve_bit), Err(DecodeError::Unsupported(_))));
    }

    #[test]
    fn rejects_files_without_image_data() {
        assert!(decode(b"not a jpeg").is_err());
        assert!(decode(&[0xFF, SOI, 0xFF, EOI]).is_err());
    }
}
