//! Module: mirante_format_jpeg::lossless
//! Purpose: Decodes lossless (SOF3) JPEG, the compression inside DNG and some RAWs.
//! Created: 2026-11-16
//! Architecture: Shares marker and Huffman parsing with decode.rs; returns raw
//! integer samples because RAW data is not an image until demosaiced.

use crate::huffman::{BitReader, HuffmanTable};
use crate::markers::{
    Coding, DHT, DRI, EOI, Frame, SOS, Scan, Segments, Tables, is_frame_marker, parse_frame,
    parse_scan,
};
use crate::scan::{McuDecoder, ScanEnd, decode_scan};
use mirante_core::DecodeError;

const MIN_PRECISION: u8 = 2;
const MAX_PRECISION: u8 = 16;
const PREDICTORS: std::ops::RangeInclusive<usize> = 1..=7;
/// The difference category that has no extra bits and means 32768 (H.1.2.2).
const LARGEST_DIFFERENCE_SIZE: u8 = 16;
const LARGEST_DIFFERENCE: i32 = 32768;

/// Samples are interleaved per component, row by row, at the frame's precision.
#[derive(Debug)]
pub struct LosslessFrame {
    pub width: u32,
    pub height: u32,
    pub components: u8,
    pub precision: u8,
    pub samples: Vec<u16>,
}

/// Decodes a lossless JPEG stream. Frames whose components have different
/// sampling factors are reported unsupported, since their samples cannot be
/// interleaved. Damaged or truncated data leaves zero samples behind.
pub fn decode_lossless(bytes: &[u8]) -> Result<LosslessFrame, DecodeError> {
    let mut segments = Segments::new(bytes)?;
    let mut tables = Tables::default();
    let mut state: Option<Samples> = None;
    let mut decoded_mcus = 0;

    while let Some(segment) = segments.next_segment()? {
        match segment.marker {
            EOI => break,
            DHT => tables.read_huffman(segment.payload)?,
            DRI => tables.read_restart(segment.payload)?,
            SOS => {
                let Some(samples) = state.as_mut() else {
                    return Err(DecodeError::corrupt("JPEG scan before the frame header"));
                };
                let scan = parse_scan(segment.payload, &samples.frame)?;
                let end = samples.decode_scan(&scan, &tables, bytes, segments.position())?;
                decoded_mcus += end.decoded_mcus;
                segments.seek(end.position);
            }
            marker if is_frame_marker(marker) => {
                if state.is_some() {
                    break;
                }
                state = Some(Samples::new(parse_frame(marker, segment.payload)?)?);
            }
            _ => {}
        }
    }
    let Some(state) = state else {
        return Err(DecodeError::corrupt("JPEG has no frame header"));
    };
    if decoded_mcus == 0 {
        return Err(DecodeError::corrupt("JPEG has no decodable image data"));
    }
    Ok(state.into_frame())
}

struct Samples {
    frame: Frame,
    /// Interleaved, still reduced by each component's point transform.
    values: Vec<u16>,
    point_transforms: Vec<u8>,
}

impl Samples {
    fn new(frame: Frame) -> Result<Self, DecodeError> {
        if frame.coding != Coding::Lossless {
            return Err(DecodeError::unsupported("not a lossless JPEG"));
        }
        if !(MIN_PRECISION..=MAX_PRECISION).contains(&frame.precision) {
            return Err(DecodeError::corrupt("bad lossless JPEG precision"));
        }
        let subsampled = frame.components.iter().any(|c| (c.h, c.v) != (1, 1));
        if frame.components.len() > 1 && subsampled {
            return Err(DecodeError::unsupported("subsampled lossless JPEG"));
        }
        let count = DecodeError::check_size(frame.width, frame.height)? * frame.components.len();
        let point_transforms = vec![0; frame.components.len()];
        Ok(Self { frame, values: vec![0; count], point_transforms })
    }

    fn decode_scan(
        &mut self,
        scan: &Scan,
        tables: &Tables,
        data: &[u8],
        start: usize,
    ) -> Result<ScanEnd, DecodeError> {
        if !PREDICTORS.contains(&scan.start) || scan.low >= self.frame.precision {
            return Err(DecodeError::corrupt("bad lossless JPEG scan header"));
        }
        let mut components = Vec::with_capacity(scan.components.len());
        for component in &scan.components {
            self.point_transforms[component.index] = scan.low;
            components.push((component.index, tables.dc(component.dc_slot)?));
        }
        let width = self.frame.width as usize;
        let mut state = LosslessScan {
            values: &mut self.values,
            width,
            stride: self.frame.components.len(),
            components,
            predictor: scan.start,
            initial: 1 << (self.frame.precision - scan.low - 1),
            interval_start: 0,
        };
        let mcu_count = width * self.frame.height as usize;
        Ok(decode_scan(data, start, tables.restart_interval, mcu_count, &mut state))
    }

    fn into_frame(self) -> LosslessFrame {
        let stride = self.point_transforms.len();
        let mut samples = self.values;
        for (index, sample) in samples.iter_mut().enumerate() {
            *sample <<= self.point_transforms[index % stride];
        }
        LosslessFrame {
            width: self.frame.width,
            height: self.frame.height,
            components: stride as u8,
            precision: self.frame.precision,
            samples,
        }
    }
}

/// One scan's view of the samples: an MCU is one sample of each scan component.
struct LosslessScan<'s, 't> {
    values: &'s mut [u16],
    width: usize,
    /// Components per pixel in `values`.
    stride: usize,
    components: Vec<(usize, &'t HuffmanTable)>,
    predictor: usize,
    /// Prediction for the first sample of each restart interval.
    initial: i32,
    interval_start: usize,
}

impl LosslessScan<'_, '_> {
    /// Prediction rules of ITU T.81 H.1.2.1, restarted at each restart interval.
    fn predict(&self, mcu: usize, component: usize) -> i32 {
        let at = |mcu: usize| i32::from(self.values[mcu * self.stride + component]);
        let (x, y) = (mcu % self.width, mcu / self.width);

        if mcu == self.interval_start {
            return self.initial;
        }
        if y == self.interval_start / self.width {
            return at(mcu - 1);
        }
        if x == 0 {
            return at(mcu - self.width);
        }
        let (left, above, diagonal) = (at(mcu - 1), at(mcu - self.width), at(mcu - self.width - 1));
        match self.predictor {
            1 => left,
            2 => above,
            3 => diagonal,
            4 => left + above - diagonal,
            5 => left + ((above - diagonal) >> 1),
            6 => above + ((left - diagonal) >> 1),
            _ => (left + above) >> 1,
        }
    }
}

impl McuDecoder for LosslessScan<'_, '_> {
    fn decode_mcu(&mut self, reader: &mut BitReader<'_>, mcu: usize) -> Result<(), DecodeError> {
        for &(component, table) in &self.components {
            let size = reader.decode(table)?;
            let difference = match size {
                LARGEST_DIFFERENCE_SIZE => LARGEST_DIFFERENCE,
                0..LARGEST_DIFFERENCE_SIZE => reader.receive_extend(u32::from(size)),
                _ => return Err(DecodeError::corrupt("bad lossless JPEG difference")),
            };
            // Reconstruction is modulo 2^16 (H.1.2.1).
            let value = self.predict(mcu, component).wrapping_add(difference) as u16;
            self.values[mcu * self.stride + component] = value;
        }
        Ok(())
    }

    fn restart(&mut self, next_mcu: usize) {
        self.interval_start = next_mcu;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::{BitWriter, HuffmanCodes, HuffmanSpec, magnitude, write_segment};
    use crate::markers::{RST0, SOF3, SOI};

    const DIFFERENCE_SIZES: [u8; 17] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
    /// All seventeen difference categories as 5-bit codes.
    const TABLE: HuffmanSpec = HuffmanSpec {
        counts: [0, 0, 0, 0, 17, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        values: &DIFFERENCE_SIZES,
    };

    struct Layout {
        width: usize,
        components: usize,
        precision: u8,
        predictor: usize,
        point_transform: u8,
        restart: usize,
    }

    /// Encodes interleaved samples, restating the prediction rules of ITU T.81 H.1.2.1.
    fn encode(samples: &[u16], layout: &Layout) -> Vec<u8> {
        let Layout { width, components, precision, predictor, point_transform, restart } = *layout;
        let height = samples.len() / width / components;
        let mut out = vec![0xFF, SOI];
        let mut frame = vec![precision];
        frame.extend((height as u16).to_be_bytes());
        frame.extend((width as u16).to_be_bytes());
        frame.push(components as u8);
        let mut scan = vec![components as u8];
        for id in 1..=components as u8 {
            frame.extend([id, 0x11, 0]);
            scan.extend([id, 0x00]);
        }
        scan.extend([predictor as u8, 0, point_transform]);
        write_segment(&mut out, SOF3, &frame);
        write_segment(&mut out, DHT, &TABLE.segment_entry(0));
        write_segment(&mut out, DRI, &(restart as u16).to_be_bytes());
        write_segment(&mut out, SOS, &scan);

        let codes = HuffmanCodes::new(&TABLE);
        let reduced: Vec<i32> = samples.iter().map(|&s| i32::from(s >> point_transform)).collect();
        let mut writer = BitWriter::new(out);
        for mcu in 0..width * height {
            if restart > 0 && mcu > 0 && mcu % restart == 0 {
                writer.marker(RST0 + ((mcu / restart - 1) % 8) as u8);
            }
            let start = mcu.checked_div(restart).map_or(0, |interval| interval * restart);
            for c in 0..components {
                let at = |m: usize| reduced[m * components + c];
                let (ra, rb, rc) =
                    (mcu.wrapping_sub(1), mcu.wrapping_sub(width), mcu.wrapping_sub(width + 1));
                let prediction = if mcu == start {
                    1 << (precision - point_transform - 1)
                } else if mcu / width == start / width {
                    at(ra)
                } else if mcu % width == 0 {
                    at(rb)
                } else {
                    match predictor {
                        1 => at(ra),
                        2 => at(rb),
                        3 => at(rc),
                        4 => at(ra) + at(rb) - at(rc),
                        5 => at(ra) + ((at(rb) - at(rc)) >> 1),
                        6 => at(rb) + ((at(ra) - at(rc)) >> 1),
                        _ => (at(ra) + at(rb)) >> 1,
                    }
                };
                let difference = (at(mcu) - prediction).rem_euclid(1 << 16);
                if difference == 1 << 15 {
                    codes.write(&mut writer, 16);
                } else {
                    let difference =
                        if difference > 1 << 15 { difference - (1 << 16) } else { difference };
                    let (size, bits) = magnitude(difference);
                    codes.write(&mut writer, size as u8);
                    writer.write(bits, size);
                }
            }
        }
        writer.marker(EOI);
        writer.bytes
    }

    fn samples(count: usize, modulus: u32) -> Vec<u16> {
        (0..count as u32).map(|i| ((i.wrapping_mul(2_654_435_761) >> 7) % modulus) as u16).collect()
    }

    #[test]
    fn decodes_every_predictor() {
        let source = samples(5 * 4, 256);
        for predictor in PREDICTORS {
            let layout = Layout {
                width: 5,
                components: 1,
                precision: 8,
                predictor,
                point_transform: 0,
                restart: 0,
            };
            let frame = decode_lossless(&encode(&source, &layout)).unwrap();
            assert_eq!((frame.width, frame.height, frame.precision), (5, 4, 8));
            assert_eq!(frame.samples, source, "predictor {predictor}");
        }
    }

    #[test]
    fn decodes_16_bit_interleaved_components_with_restarts() {
        let mut source = samples(4 * 3 * 2, 1 << 16);
        source[0] = 0;
        let layout = Layout {
            width: 4,
            components: 2,
            precision: 16,
            predictor: 6,
            point_transform: 0,
            restart: 4,
        };
        let frame = decode_lossless(&encode(&source, &layout)).unwrap();
        assert_eq!(frame.components, 2);
        assert_eq!(frame.samples, source);
    }

    #[test]
    fn applies_the_point_transform() {
        let source = samples(6 * 2, 1 << 12);
        let layout = Layout {
            width: 6,
            components: 1,
            precision: 12,
            predictor: 4,
            point_transform: 2,
            restart: 0,
        };
        let bytes = encode(&source, &layout);
        let frame = decode_lossless(&bytes).unwrap();
        let expected: Vec<u16> = source.iter().map(|&s| s >> 2 << 2).collect();
        assert_eq!(frame.samples, expected);
        let image = crate::decode(&bytes).unwrap();
        assert_eq!((image.width(), image.height()), (6, 2));
    }
}
