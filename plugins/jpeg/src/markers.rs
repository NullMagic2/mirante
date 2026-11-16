//! Module: mirante_format_jpeg::markers
//! Purpose: Walks JPEG marker segments and parses frame, scan and table headers.
//! Created: 2026-11-16
//! Architecture: Shared by the DCT decoder (decode.rs) and the lossless decoder
//! (lossless.rs). Entropy-coded data between segments is handled by scan.rs.

use crate::huffman::HuffmanTable;
use mirante_codec::{ByteReader, Endian};
use mirante_core::DecodeError;

pub const SOF0: u8 = 0xC0;
pub const SOF1: u8 = 0xC1;
pub const SOF2: u8 = 0xC2;
pub const SOF3: u8 = 0xC3;
pub const DHT: u8 = 0xC4;
pub const JPG: u8 = 0xC8;
pub const DAC: u8 = 0xCC;
pub const SOF15: u8 = 0xCF;
pub const RST0: u8 = 0xD0;
pub const RST7: u8 = 0xD7;
pub const SOI: u8 = 0xD8;
pub const EOI: u8 = 0xD9;
pub const SOS: u8 = 0xDA;
pub const DQT: u8 = 0xDB;
pub const DRI: u8 = 0xDD;
pub const APP0: u8 = 0xE0;
pub const APP1: u8 = 0xE1;
pub const APP2: u8 = 0xE2;
pub const APP14: u8 = 0xEE;
pub const APP15: u8 = 0xEF;
const TEM: u8 = 0x01;

/// Natural (row-major) index of each zig-zag position.
pub const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// Table slots per kind; a DHT or DQT may name destinations 0..=3.
const TABLE_SLOTS: usize = 4;
/// Gray, YCbCr/RGB and CMYK/YCCK need at most four; more is not a colour image.
const MAX_COMPONENTS: usize = 4;
const MAX_SAMPLING: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coding {
    Baseline,
    Progressive,
    Lossless,
}

#[derive(Clone, Debug)]
pub struct FrameComponent {
    pub id: u8,
    pub h: usize,
    pub v: usize,
    pub quant_slot: usize,
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub coding: Coding,
    pub precision: u8,
    pub width: u32,
    pub height: u32,
    pub components: Vec<FrameComponent>,
    pub h_max: usize,
    pub v_max: usize,
}

impl Frame {
    /// A component's size in samples before padding: the image scaled by its sampling.
    pub fn component_size(&self, index: usize) -> (usize, usize) {
        let component = &self.components[index];
        let width = (self.width as usize * component.h).div_ceil(self.h_max);
        let height = (self.height as usize * component.v).div_ceil(self.v_max);
        (width, height)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ScanComponent {
    /// Index into `Frame::components`.
    pub index: usize,
    pub dc_slot: usize,
    pub ac_slot: usize,
}

/// A scan header. Lossless JPEG reuses `start` as the predictor and `low` as the
/// point transform.
#[derive(Clone, Debug)]
pub struct Scan {
    pub components: Vec<ScanComponent>,
    pub start: usize,
    pub end: usize,
    pub high: u8,
    pub low: u8,
}

pub fn is_frame_marker(marker: u8) -> bool {
    (SOF0..=SOF15).contains(&marker) && !matches!(marker, DHT | JPG | DAC)
}

pub fn parse_frame(marker: u8, payload: &[u8]) -> Result<Frame, DecodeError> {
    let coding = match marker {
        SOF0 | SOF1 => Coding::Baseline,
        SOF2 => Coding::Progressive,
        SOF3 => Coding::Lossless,
        0xC5..=0xC7 => return Err(DecodeError::unsupported("hierarchical JPEG")),
        _ => return Err(DecodeError::unsupported("arithmetic-coded JPEG")),
    };
    let mut reader = ByteReader::new(payload, Endian::Big);
    let precision = reader.u8()?;
    let height = u32::from(reader.u16()?);
    let width = u32::from(reader.u16()?);
    let count = usize::from(reader.u8()?);

    if height == 0 {
        return Err(DecodeError::unsupported("JPEG height defined by a DNL marker"));
    }
    if width == 0 || count == 0 {
        return Err(DecodeError::corrupt("JPEG frame has no width or no components"));
    }
    if count > MAX_COMPONENTS {
        return Err(DecodeError::unsupported(format!("JPEG with {count} components")));
    }
    let mut components: Vec<FrameComponent> = Vec::with_capacity(count);

    for _ in 0..count {
        let id = reader.u8()?;
        let sampling = reader.u8()?;
        let (h, v) = (usize::from(sampling >> 4), usize::from(sampling & 15));
        let quant_slot = usize::from(reader.u8()?);

        if !(1..=MAX_SAMPLING).contains(&h) || !(1..=MAX_SAMPLING).contains(&v) {
            return Err(DecodeError::corrupt("bad JPEG sampling factors"));
        }
        if quant_slot >= TABLE_SLOTS || components.iter().any(|c| c.id == id) {
            return Err(DecodeError::corrupt("bad JPEG component definition"));
        }
        components.push(FrameComponent { id, h, v, quant_slot });
    }
    let h_max = components.iter().map(|c| c.h).max().unwrap_or(1);
    let v_max = components.iter().map(|c| c.v).max().unwrap_or(1);
    Ok(Frame { coding, precision, width, height, components, h_max, v_max })
}

pub fn parse_scan(payload: &[u8], frame: &Frame) -> Result<Scan, DecodeError> {
    let mut reader = ByteReader::new(payload, Endian::Big);
    let count = usize::from(reader.u8()?);

    if count == 0 || count > frame.components.len() {
        return Err(DecodeError::corrupt("bad JPEG scan component count"));
    }
    let mut components: Vec<ScanComponent> = Vec::with_capacity(count);

    for _ in 0..count {
        let id = reader.u8()?;
        let slots = reader.u8()?;
        let index = frame.components.iter().position(|c| c.id == id);
        let Some(index) = index.filter(|&i| components.iter().all(|c| c.index != i)) else {
            return Err(DecodeError::corrupt("JPEG scan names an unknown component"));
        };
        let (dc_slot, ac_slot) = (usize::from(slots >> 4), usize::from(slots & 15));

        if dc_slot >= TABLE_SLOTS || ac_slot >= TABLE_SLOTS {
            return Err(DecodeError::corrupt("bad JPEG Huffman table selector"));
        }
        components.push(ScanComponent { index, dc_slot, ac_slot });
    }
    let start = usize::from(reader.u8()?);
    let end = usize::from(reader.u8()?);
    let approximation = reader.u8()?;
    Ok(Scan { components, start, end, high: approximation >> 4, low: approximation & 15 })
}

/// Huffman and quantization tables plus the restart interval, as currently defined.
#[derive(Default)]
pub struct Tables {
    dc: [Option<HuffmanTable>; TABLE_SLOTS],
    ac: [Option<HuffmanTable>; TABLE_SLOTS],
    /// Quantization tables in natural order.
    quant: [Option<[u16; 64]>; TABLE_SLOTS],
    /// MCUs between restart markers; 0 means none.
    pub restart_interval: usize,
}

impl Tables {
    pub fn read_huffman(&mut self, payload: &[u8]) -> Result<(), DecodeError> {
        let mut reader = ByteReader::new(payload, Endian::Big);

        while reader.remaining() > 0 {
            let target = reader.u8()?;
            let (class, slot) = (target >> 4, usize::from(target & 15));
            let counts: [u8; 16] = reader.bytes(16)?.try_into().expect("16 bytes were read");
            let total = counts.iter().map(|&c| usize::from(c)).sum();
            let table = HuffmanTable::new(&counts, reader.bytes(total)?)?;

            match (class, slot < TABLE_SLOTS) {
                (0, true) => self.dc[slot] = Some(table),
                (1, true) => self.ac[slot] = Some(table),
                _ => return Err(DecodeError::corrupt("bad JPEG Huffman table destination")),
            }
        }
        Ok(())
    }

    pub fn read_quant(&mut self, payload: &[u8]) -> Result<(), DecodeError> {
        let mut reader = ByteReader::new(payload, Endian::Big);

        while reader.remaining() > 0 {
            let target = reader.u8()?;
            let (wide, slot) = (target >> 4, usize::from(target & 15));

            if wide > 1 || slot >= TABLE_SLOTS {
                return Err(DecodeError::corrupt("bad JPEG quantization table"));
            }
            let mut table = [0; 64];
            for &natural in &ZIGZAG {
                table[natural] = if wide == 1 { reader.u16()? } else { u16::from(reader.u8()?) };
            }
            self.quant[slot] = Some(table);
        }
        Ok(())
    }

    pub fn read_restart(&mut self, payload: &[u8]) -> Result<(), DecodeError> {
        self.restart_interval = usize::from(ByteReader::new(payload, Endian::Big).u16()?);
        Ok(())
    }

    pub fn dc(&self, slot: usize) -> Result<&HuffmanTable, DecodeError> {
        self.dc[slot].as_ref().ok_or_else(|| DecodeError::corrupt("missing JPEG Huffman table"))
    }

    pub fn ac(&self, slot: usize) -> Result<&HuffmanTable, DecodeError> {
        self.ac[slot].as_ref().ok_or_else(|| DecodeError::corrupt("missing JPEG Huffman table"))
    }

    pub fn quant(&self, slot: usize) -> Result<[u16; 64], DecodeError> {
        self.quant[slot].ok_or_else(|| DecodeError::corrupt("missing JPEG quantization table"))
    }
}

pub struct Segment<'a> {
    pub marker: u8,
    pub payload: &'a [u8],
}

/// Iterates over marker segments, skipping garbage and stray restart markers.
pub struct Segments<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Segments<'a> {
    pub fn new(data: &'a [u8]) -> Result<Self, DecodeError> {
        if !data.starts_with(&[0xFF, SOI]) {
            return Err(DecodeError::corrupt("missing JPEG start-of-image marker"));
        }
        Ok(Self { data, position: 2 })
    }

    /// Where the bytes after the last segment start; a scan's entropy data begins here.
    pub fn position(&self) -> usize {
        self.position
    }

    /// Continues after entropy-coded data that ended at `position`.
    pub fn seek(&mut self, position: usize) {
        self.position = position;
    }

    /// The next segment, or None when the data ends, cleanly or truncated.
    pub fn next_segment(&mut self) -> Result<Option<Segment<'a>>, DecodeError> {
        while let Some((at, marker)) = find_marker(self.data, self.position) {
            self.position = at + 2;

            if (RST0..=RST7).contains(&marker) {
                continue;
            }
            if matches!(marker, SOI | EOI | TEM) {
                return Ok(Some(Segment { marker, payload: &[] }));
            }
            let mut reader = ByteReader::new(self.data, Endian::Big).at(self.position)?;
            let length = usize::from(reader.u16()?);

            if length < 2 {
                return Err(DecodeError::corrupt("bad JPEG segment length"));
            }
            let Ok(payload) = reader.bytes(length - 2) else {
                break;
            };
            self.position = reader.position();
            return Ok(Some(Segment { marker, payload }));
        }
        self.position = self.data.len();
        Ok(None)
    }
}

/// Position and code of the first marker at or after `from`; 0xFF00 is stuffing.
pub fn find_marker(data: &[u8], from: usize) -> Option<(usize, u8)> {
    let tail = data.get(from..)?;
    tail.windows(2)
        .position(|pair| pair[0] == 0xFF && pair[1] != 0 && pair[1] != 0xFF)
        .map(|offset| (from + offset, tail[offset + 1]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zigzag_walks_the_anti_diagonals() {
        let mut expected = Vec::new();
        for diagonal in 0..15_usize {
            let rows = diagonal.saturating_sub(7)..=diagonal.min(7);
            let mut cells: Vec<usize> = rows.map(|row| row * 8 + diagonal - row).collect();
            if diagonal % 2 == 0 {
                cells.reverse();
            }
            expected.extend(cells);
        }
        assert_eq!(ZIGZAG.to_vec(), expected);
    }

    #[test]
    fn segments_skip_fill_bytes_and_garbage() {
        let data = [0xFF, SOI, 0x12, 0xFF, 0xFF, 0xFE, 0, 3, 7, 0xFF, EOI];
        let mut segments = Segments::new(&data).unwrap();
        let comment = segments.next_segment().unwrap().unwrap();
        assert_eq!((comment.marker, comment.payload), (0xFE, &[7][..]));
        assert_eq!(segments.next_segment().unwrap().unwrap().marker, EOI);
        assert!(segments.next_segment().unwrap().is_none());
    }
}
