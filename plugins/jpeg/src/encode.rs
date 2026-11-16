//! Module: mirante_format_jpeg::encode
//! Purpose: Writes an sRGB Image as a baseline JFIF JPEG with 4:2:0 chroma.
//! Created: 2026-11-16
//! Architecture: Uses dct.rs for the forward transform and the Annex K Huffman
//! tables through huffman.rs's canonical code builder. Alpha is composited over
//! white because JPEG has no transparency.

use crate::dct;
use crate::huffman::canonical_codes;
use crate::markers::{APP0, DHT, DQT, DRI, EOI, RST0, SOF0, SOI, SOS, ZIGZAG};
use mirante_core::{EncodeError, Image};

pub const QUALITY: u32 = 92;
const MAX_DIMENSION: u32 = u16::MAX as u32;
const MCU_SIZE: usize = 16;
const BLOCK_SIZE: usize = 8;
const RESTART_CYCLE: usize = 8;
/// Keeps DC differences within the 11-bit categories of the Annex K tables.
const MAX_DC: i32 = 1023;
const MAX_AC: i32 = 1023;
const END_OF_BLOCK: u8 = 0x00;
const ZERO_RUN: u8 = 0xF0;

/// Annex K.1 quantization tables, natural order, for quality 50.
const LUMA_QUANT: [u16; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113,
    92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];
const CHROMA_QUANT: [u16; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];

/// A Huffman table as stored in a DHT segment: code counts per length, then values.
pub(crate) struct HuffmanSpec {
    pub counts: [u8; 16],
    pub values: &'static [u8],
}

/// Annex K.3 tables.
pub(crate) const DC_LUMA: HuffmanSpec = HuffmanSpec {
    counts: [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0],
    values: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
};
pub(crate) const DC_CHROMA: HuffmanSpec = HuffmanSpec {
    counts: [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0],
    values: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
};
pub(crate) const AC_LUMA: HuffmanSpec = HuffmanSpec {
    counts: [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7D],
    values: &[
        0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61,
        0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xA1, 0x08, 0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52,
        0xD1, 0xF0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0A, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x25,
        0x26, 0x27, 0x28, 0x29, 0x2A, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45,
        0x46, 0x47, 0x48, 0x49, 0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64,
        0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x83,
        0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99,
        0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6,
        0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3,
        0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8,
        0xE9, 0xEA, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA,
    ],
};
pub(crate) const AC_CHROMA: HuffmanSpec = HuffmanSpec {
    counts: [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77],
    values: &[
        0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61,
        0x71, 0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xA1, 0xB1, 0xC1, 0x09, 0x23, 0x33,
        0x52, 0xF0, 0x15, 0x62, 0x72, 0xD1, 0x0A, 0x16, 0x24, 0x34, 0xE1, 0x25, 0xF1, 0x17, 0x18,
        0x19, 0x1A, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44,
        0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63,
        0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A,
        0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97,
        0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4,
        0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA,
        0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7,
        0xE8, 0xE9, 0xEA, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA,
    ],
};

impl HuffmanSpec {
    /// The DHT entry for this table in destination `class << 4 | slot`.
    pub(crate) fn segment_entry(&self, destination: u8) -> Vec<u8> {
        [&[destination][..], &self.counts, self.values].concat()
    }
}

/// (code, length) for every symbol value.
pub(crate) struct HuffmanCodes([(u16, u8); 256]);

impl HuffmanCodes {
    pub(crate) fn new(spec: &HuffmanSpec) -> Self {
        let codes = canonical_codes(&spec.counts).expect("built-in Huffman tables are complete");
        let mut table = [(0, 0); 256];
        for (&value, code) in spec.values.iter().zip(codes) {
            table[usize::from(value)] = code;
        }
        Self(table)
    }

    pub(crate) fn write(&self, writer: &mut BitWriter, symbol: u8) {
        let (code, length) = self.0[usize::from(symbol)];
        writer.write(u32::from(code), u32::from(length));
    }
}

/// Packs bits most significant first, stuffing a zero byte after every 0xFF.
pub(crate) struct BitWriter {
    pub bytes: Vec<u8>,
    buffer: u32,
    count: u32,
}

impl BitWriter {
    /// Continues after the header bytes already in `bytes`.
    pub(crate) fn new(bytes: Vec<u8>) -> Self {
        Self { bytes, buffer: 0, count: 0 }
    }

    /// Appends the low `length` bits of `bits` (length at most 16).
    pub(crate) fn write(&mut self, bits: u32, length: u32) {
        self.buffer = (self.buffer << length) | (bits & ((1 << length) - 1));
        self.count += length;
        while self.count >= 8 {
            self.count -= 8;
            let byte = (self.buffer >> self.count) as u8;
            self.bytes.push(byte);
            if byte == 0xFF {
                self.bytes.push(0);
            }
        }
    }

    /// Pads the last byte with one bits, as the standard asks.
    pub(crate) fn flush(&mut self) {
        if self.count > 0 {
            self.write(0x7F, 8 - self.count);
        }
    }

    pub(crate) fn marker(&mut self, marker: u8) {
        self.flush();
        self.bytes.extend([0xFF, marker]);
    }
}

/// The JPEG size category of a value and its extra bits (F.1.2.1).
pub(crate) fn magnitude(value: i32) -> (u32, u32) {
    let size = 32 - value.unsigned_abs().leading_zeros();
    let bits = if value < 0 { value - 1 } else { value };
    (size, bits as u32 & ((1 << size) - 1))
}

pub(crate) fn write_segment(out: &mut Vec<u8>, marker: u8, payload: &[u8]) {
    out.extend([0xFF, marker]);
    out.extend(((payload.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(payload);
}

pub fn encode(image: &Image) -> Result<Vec<u8>, EncodeError> {
    encode_with_restarts(image, 0)
}

/// Encodes with a restart marker every `restart_interval` MCUs (0 for none).
pub(crate) fn encode_with_restarts(
    image: &Image,
    restart_interval: u16,
) -> Result<Vec<u8>, EncodeError> {
    let (width, height) = (image.width(), image.height());
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(EncodeError(format!("JPEG cannot store {width}x{height} pixels")));
    }
    let (width, height) = (width as usize, height as usize);
    let [luma, cb, cr] = ycbcr_planes(image);
    let chroma_size = (width.div_ceil(2), height.div_ceil(2));
    let chroma = [halve(&cb, (width, height)), halve(&cr, (width, height))];
    let luma_quant = scaled_quant(&LUMA_QUANT);
    let chroma_quant = scaled_quant(&CHROMA_QUANT);

    let mut out = vec![0xFF, SOI];
    write_segment(&mut out, APP0, b"JFIF\0\x01\x01\x00\x00\x01\x00\x01\x00\x00");
    let mut tables = vec![0];
    tables.extend(ZIGZAG.map(|i| luma_quant[i] as u8));
    tables.push(1);
    tables.extend(ZIGZAG.map(|i| chroma_quant[i] as u8));
    write_segment(&mut out, DQT, &tables);
    let [h0, h1] = (height as u16).to_be_bytes();
    let [w0, w1] = (width as u16).to_be_bytes();
    write_segment(&mut out, SOF0, &[8, h0, h1, w0, w1, 3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
    let huffman = [
        DC_LUMA.segment_entry(0x00),
        AC_LUMA.segment_entry(0x10),
        DC_CHROMA.segment_entry(0x01),
        AC_CHROMA.segment_entry(0x11),
    ];
    write_segment(&mut out, DHT, &huffman.concat());
    if restart_interval > 0 {
        write_segment(&mut out, DRI, &restart_interval.to_be_bytes());
    }
    write_segment(&mut out, SOS, &[3, 1, 0x00, 2, 0x11, 3, 0x11, 0, 63, 0]);

    let luma_table = dct::forward_table(&luma_quant);
    let chroma_table = dct::forward_table(&chroma_quant);
    let luma_codes = (HuffmanCodes::new(&DC_LUMA), HuffmanCodes::new(&AC_LUMA));
    let chroma_codes = (HuffmanCodes::new(&DC_CHROMA), HuffmanCodes::new(&AC_CHROMA));
    let mut writer = BitWriter::new(out);
    let mut predictors = [0; 3];
    let mcus_x = width.div_ceil(MCU_SIZE);
    let mcu_count = mcus_x * height.div_ceil(MCU_SIZE);
    let interval = usize::from(restart_interval);

    for mcu in 0..mcu_count {
        if interval > 0 && mcu > 0 && mcu % interval == 0 {
            writer.marker(RST0 + ((mcu / interval - 1) % RESTART_CYCLE) as u8);
            predictors = [0; 3];
        }
        let (x, y) = ((mcu % mcus_x) * MCU_SIZE, (mcu / mcus_x) * MCU_SIZE);
        for (dy, dx) in [(0, 0), (0, BLOCK_SIZE), (BLOCK_SIZE, 0), (BLOCK_SIZE, BLOCK_SIZE)] {
            let block = quantize(&luma, (width, height), (x + dx, y + dy), &luma_table);
            write_block(&mut writer, &block, &mut predictors[0], &luma_codes);
        }
        for (plane, predictor) in chroma.iter().zip(&mut predictors[1..]) {
            let block = quantize(plane, chroma_size, (x / 2, y / 2), &chroma_table);
            write_block(&mut writer, &block, predictor, &chroma_codes);
        }
    }
    writer.marker(EOI);
    Ok(writer.bytes)
}

/// libjpeg's quality scaling of the Annex K tables.
fn scaled_quant(base: &[u16; 64]) -> [u16; 64] {
    let scale = if QUALITY < 50 { 5000 / QUALITY } else { 200 - 2 * QUALITY };
    base.map(|q| ((u32::from(q) * scale + 50) / 100).clamp(1, 255) as u16)
}

/// Full-resolution Y, Cb and Cr planes in 0..255, alpha composited over white.
fn ycbcr_planes(image: &Image) -> [Vec<f32>; 3] {
    let mut planes = [(); 3].map(|_| Vec::with_capacity(image.pixels().len()));
    for &[r, g, b, a] in image.pixels() {
        let a = a.clamp(0.0, 1.0);
        let [r, g, b] = [r, g, b].map(|c| (c.clamp(0.0, 1.0) * a + 1.0 - a) * 255.0);
        planes[0].push(0.299 * r + 0.587 * g + 0.114 * b);
        planes[1].push(-0.168_736 * r - 0.331_264 * g + 0.5 * b + 128.0);
        planes[2].push(0.5 * r - 0.418_688 * g - 0.081_312 * b + 128.0);
    }
    planes
}

/// Averages 2x2 neighbourhoods, repeating the last row and column of odd sizes.
fn halve(plane: &[f32], (width, height): (usize, usize)) -> Vec<f32> {
    let mut out = Vec::with_capacity(width.div_ceil(2) * height.div_ceil(2));
    for y in (0..height).step_by(2) {
        let below = (y + 1).min(height - 1);
        for x in (0..width).step_by(2) {
            let right = (x + 1).min(width - 1);
            let sum = plane[y * width + x]
                + plane[y * width + right]
                + plane[below * width + x]
                + plane[below * width + right];
            out.push(sum / 4.0);
        }
    }
    out
}

/// Transforms and quantizes the 8x8 block at `origin`, repeating edge samples.
fn quantize(
    plane: &[f32],
    (width, height): (usize, usize),
    (x, y): (usize, usize),
    table: &[f32; 64],
) -> [i32; 64] {
    let samples = std::array::from_fn(|i| {
        let column = (x + i % BLOCK_SIZE).min(width - 1);
        let row = (y + i / BLOCK_SIZE).min(height - 1);
        plane[row * width + column] - 128.0
    });
    let coefficients = dct::forward(&samples);
    std::array::from_fn(|i| {
        let limit = if i == 0 { MAX_DC } else { MAX_AC };
        ((coefficients[i] * table[i]).round() as i32).clamp(-limit, limit)
    })
}

fn write_block(
    writer: &mut BitWriter,
    block: &[i32; 64],
    predictor: &mut i32,
    (dc_codes, ac_codes): &(HuffmanCodes, HuffmanCodes),
) {
    let (size, bits) = magnitude(block[0] - *predictor);
    *predictor = block[0];
    dc_codes.write(writer, size as u8);
    writer.write(bits, size);

    let mut run = 0;
    for &natural in &ZIGZAG[1..] {
        let value = block[natural];
        if value == 0 {
            run += 1;
            continue;
        }
        while run >= 16 {
            ac_codes.write(writer, ZERO_RUN);
            run -= 16;
        }
        let (size, bits) = magnitude(value);
        ac_codes.write(writer, (run << 4 | size) as u8);
        writer.write(bits, size);
        run = 0;
    }
    if run > 0 {
        ac_codes.write(writer, END_OF_BLOCK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante_core::ColorSpace;

    fn gradient(width: u32, height: u32) -> Image {
        let pixels = (0..width * height)
            .map(|i| {
                let (x, y) =
                    ((i % width) as f32 / width as f32, (i / width) as f32 / height as f32);
                [x, y, 1.0 - x * y, 1.0]
            })
            .collect();
        Image::new(width, height, pixels, ColorSpace::Srgb).unwrap()
    }

    fn mean_error(a: &Image, b: &Image) -> f32 {
        let pairs = a.pixels().iter().zip(b.pixels());
        let total: f32 = pairs.map(|(p, q)| (0..3).map(|c| (p[c] - q[c]).abs()).sum::<f32>()).sum();
        total / (a.pixels().len() * 3) as f32
    }

    #[test]
    fn round_trips_odd_sizes() {
        let image = gradient(37, 23);
        let decoded = crate::decode(&encode(&image).unwrap()).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (37, 23));
        assert!(mean_error(&image, &decoded) < 2.0 / 255.0);
    }

    #[test]
    fn composites_alpha_over_white() {
        let image = Image::new(8, 8, vec![[0.0, 0.0, 0.0, 0.0]; 64], ColorSpace::Srgb).unwrap();
        let decoded = crate::decode(&encode(&image).unwrap()).unwrap();
        assert!(decoded.pixels().iter().all(|p| p[0] > 0.99 && p[2] > 0.99));
    }

    #[test]
    fn magnitude_follows_the_standard() {
        assert_eq!(magnitude(0), (0, 0));
        assert_eq!(magnitude(-1), (1, 0));
        assert_eq!(magnitude(5), (3, 5));
        assert_eq!(magnitude(-5), (3, 2));
    }
}
