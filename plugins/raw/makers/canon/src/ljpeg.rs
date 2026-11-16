//! Module: mirante_raw_canon::ljpeg
//! Purpose: Row-by-row lossless JPEG decoding with Canon's sRAW quirk: the
//! luma samples of one MCU are predicted from each other in coding order,
//! which standard decoders (mirante_format_jpeg) cannot express.
//! Created: 2026-11-16
//! Architecture: Ported from dcraw by Dave Coffin (ljpeg_start, ljpeg_row).
//! Entropy data is unstuffed first, then read with mirante_raw::bits.

use mirante_core::DecodeError;
use mirante_raw::bits::{BitReader, HuffmanTable, signed_difference};

const SOI: u8 = 0xD8;
const SOF3: u8 = 0xC3;
const DHT: u8 = 0xC4;
const DRI: u8 = 0xDD;
const SOS: u8 = 0xDA;
const RST0: u8 = 0xD0;
const RST7: u8 = 0xD7;
const MAX_SAMPLES_PER_MCU: usize = 6;
const MAX_TABLES: usize = 4;
const HUFFMAN_COUNTS: usize = 16;

/// The frame as dcraw's `jhead` sees it.
pub struct LosslessJpeg {
    pub bits: u32,
    pub width: usize,
    pub height: usize,
    /// Samples per MCU, counting every luma sample of a subsampled MCU.
    pub samples_per_mcu: usize,
    /// Extra luma samples per MCU: 1 for 4:2:2 sRAW, 3 for 4:2:0, 0 otherwise.
    pub sraw: usize,
    predictor: usize,
    restart: usize,
    tables: Vec<HuffmanTable>,
    /// Table index per sample position in the MCU.
    table_of: [usize; MAX_SAMPLES_PER_MCU],
    /// Unstuffed entropy data, one entry per restart interval.
    intervals: Vec<Vec<u8>>,
}

impl LosslessJpeg {
    pub fn parse(bytes: &[u8]) -> Result<Self, DecodeError> {
        if bytes.get(..2) != Some(&[0xFF, SOI]) {
            return Err(DecodeError::corrupt("CR2 raw data is not a JPEG stream"));
        }
        let mut position = 2;
        let mut frame: Option<(u32, usize, usize, usize, usize)> = None;
        let mut tables: [Option<HuffmanTable>; MAX_TABLES] = Default::default();
        let mut restart = 0;
        loop {
            let header = bytes.get(position..position + 4).ok_or_else(truncated)?;
            let (marker, length) =
                (header[1], usize::from(u16::from_be_bytes([header[2], header[3]])));
            if header[0] != 0xFF || length < 2 {
                return Err(DecodeError::corrupt("bad JPEG marker in CR2 raw data"));
            }
            let payload = bytes.get(position + 4..position + 2 + length).ok_or_else(truncated)?;
            position += 2 + length;
            match marker {
                SOF3 => frame = Some(parse_frame(payload)?),
                DHT => read_tables(payload, &mut tables)?,
                DRI => restart = usize::from(u16::from_be_bytes([payload[0], payload[1]])),
                SOS => {
                    let Some((bits, height, width, components, sraw)) = frame else {
                        return Err(DecodeError::corrupt("JPEG scan before its frame header"));
                    };
                    let count = usize::from(*payload.first().ok_or_else(truncated)?);
                    let field = |at: usize| payload.get(at).copied().ok_or_else(truncated);
                    let predictor = usize::from(field(1 + 2 * count)?);
                    let point_transform = u32::from(field(3 + 2 * count)? & 15);
                    let bits = bits.checked_sub(point_transform).filter(|&b| b > 0);
                    let bits =
                        bits.ok_or_else(|| DecodeError::corrupt("bad JPEG point transform"))?;
                    let samples_per_mcu = components + sraw;
                    let (tables, table_of) = assign_tables(tables, sraw)?;
                    return Ok(Self {
                        bits,
                        width,
                        height,
                        samples_per_mcu,
                        sraw,
                        predictor,
                        restart,
                        tables,
                        table_of,
                        intervals: unstuff(&bytes[position..]),
                    });
                }
                _ => {}
            }
        }
    }

    /// A row decoder producing `mcus` MCUs per row.
    pub fn rows(&self, mcus: usize) -> Rows<'_> {
        let samples = mcus * self.samples_per_mcu;
        Rows {
            jpeg: self,
            mcus,
            row: 0,
            interval: 0,
            reader: BitReader::new(self.intervals.first().map_or(&[], Vec::as_slice)),
            vertical: [0; MAX_SAMPLES_PER_MCU],
            previous: vec![0; samples],
            current: vec![0; samples],
        }
    }
}

/// True when the frame's first component is subsampled (Canon sRAW/mRAW).
pub fn is_subsampled(bytes: &[u8]) -> Result<bool, DecodeError> {
    let mut position = 2;
    while let Some(header) = bytes.get(position..position + 4) {
        let length = usize::from(u16::from_be_bytes([header[2], header[3]]));
        if header[0] != 0xFF || length < 2 || header[1] == SOS {
            break;
        }
        if header[1] == SOF3 {
            let payload = bytes.get(position + 4..position + 2 + length).ok_or_else(truncated)?;
            return Ok(parse_frame(payload)?.4 > 0);
        }
        position += 2 + length;
    }
    Ok(false)
}

fn truncated() -> DecodeError {
    DecodeError::corrupt("CR2 raw JPEG stream is truncated")
}

/// Precision, height, width, components and dcraw's `sraw` extra luma count.
fn parse_frame(payload: &[u8]) -> Result<(u32, usize, usize, usize, usize), DecodeError> {
    let header = payload.get(..6).ok_or_else(truncated)?;
    let components = usize::from(header[5]);
    let sampling = *payload.get(7).ok_or_else(truncated)?;
    let blocks = usize::from(sampling >> 4) * usize::from(sampling & 15);
    let sraw = blocks.wrapping_sub(1) & 3;
    let bits = u32::from(header[0]);
    let height = usize::from(u16::from_be_bytes([header[1], header[2]]));
    let width = usize::from(u16::from_be_bytes([header[3], header[4]]));
    let samples = components + sraw;
    if !(2..=16).contains(&bits)
        || width == 0
        || height == 0
        || !(1..=MAX_SAMPLES_PER_MCU).contains(&samples)
    {
        return Err(DecodeError::corrupt("bad lossless JPEG frame header"));
    }
    Ok((bits, height, width, components, sraw))
}

fn read_tables(
    mut payload: &[u8],
    tables: &mut [Option<HuffmanTable>; MAX_TABLES],
) -> Result<(), DecodeError> {
    while let Some((&class_and_id, rest)) = payload.split_first() {
        let counts = rest.get(..HUFFMAN_COUNTS).ok_or_else(truncated)?;
        let symbols: usize = counts.iter().map(|&c| usize::from(c)).sum();
        let spec = rest.get(..HUFFMAN_COUNTS + symbols).ok_or_else(truncated)?;
        let slot = tables
            .get_mut(usize::from(class_and_id))
            .ok_or_else(|| DecodeError::unsupported("JPEG Huffman table class in CR2 data"))?;
        *slot = Some(HuffmanTable::from_counts(spec)?);
        payload = &rest[HUFFMAN_COUNTS + symbols..];
    }
    Ok(())
}

/// dcraw's table choice: table n for sample n, missing ones repeat the one
/// before; in sRAW every luma sample uses table 0 and chroma table 1.
fn assign_tables(
    tables: [Option<HuffmanTable>; MAX_TABLES],
    sraw: usize,
) -> Result<(Vec<HuffmanTable>, [usize; MAX_SAMPLES_PER_MCU]), DecodeError> {
    let present: Vec<usize> = (0..MAX_TABLES).filter(|&i| tables[i].is_some()).collect();
    if present.first() != Some(&0) {
        return Err(DecodeError::corrupt("lossless JPEG lacks Huffman table 0"));
    }
    let mut table_of = [0; MAX_SAMPLES_PER_MCU];
    for (sample, slot) in table_of.iter_mut().enumerate() {
        *slot = present.iter().copied().filter(|&t| t <= sample).max().unwrap_or(0);
    }
    if sraw > 0 {
        let chroma = table_of[1];
        for (sample, slot) in table_of.iter_mut().enumerate() {
            *slot = if sample <= sraw { 0 } else { chroma };
        }
    }
    let mut owned: Vec<Option<HuffmanTable>> = tables.into_iter().collect();
    let mut list = Vec::new();
    let mut index_of = [0; MAX_TABLES];
    for &t in &present {
        index_of[t] = list.len();
        list.push(owned[t].take().expect("present table"));
    }
    Ok((list, table_of.map(|t| index_of[t])))
}

/// Removes JPEG byte stuffing; restart markers split the data into intervals
/// and any other marker ends it.
fn unstuff(data: &[u8]) -> Vec<Vec<u8>> {
    let mut intervals = vec![Vec::with_capacity(data.len())];
    let mut bytes = data.iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        if byte != 0xFF {
            intervals.last_mut().expect("one interval").push(byte);
            continue;
        }
        match bytes.next() {
            Some(0) => intervals.last_mut().expect("one interval").push(0xFF),
            Some(RST0..=RST7) => intervals.push(Vec::new()),
            _ => break,
        }
    }
    intervals
}

/// Decodes rows one after another, as dcraw's ljpeg_row does.
pub struct Rows<'a> {
    jpeg: &'a LosslessJpeg,
    mcus: usize,
    row: usize,
    interval: usize,
    reader: BitReader<'a>,
    vertical: [i32; MAX_SAMPLES_PER_MCU],
    previous: Vec<u16>,
    current: Vec<u16>,
}

impl Rows<'_> {
    pub fn next_row(&mut self) -> Result<&[u16], DecodeError> {
        let jpeg = self.jpeg;
        if self.row >= jpeg.height {
            return Err(DecodeError::corrupt("CR2 raw data needs more JPEG rows than it has"));
        }
        let restart = jpeg.restart > 0 && (self.row * self.mcus) % jpeg.restart == 0;
        if self.row == 0 || restart {
            self.vertical = [1 << (jpeg.bits - 1); MAX_SAMPLES_PER_MCU];
            if self.row > 0 {
                self.interval += 1;
                let data = jpeg.intervals.get(self.interval).map_or(&[][..], Vec::as_slice);
                self.reader = BitReader::new(data);
            }
        }
        std::mem::swap(&mut self.previous, &mut self.current);
        let clrs = jpeg.samples_per_mcu;
        let mut luma = 0;
        for column in 0..self.mcus {
            for c in 0..clrs {
                let table = &jpeg.tables[jpeg.table_of[c]];
                let length = u32::from(table.decode(&mut self.reader)?);
                let diff = signed_difference(&mut self.reader, length);
                let at = column * clrs + c;
                let mut prediction = if jpeg.sraw > 0 && c <= jpeg.sraw && (column | c) != 0 {
                    luma
                } else if column > 0 {
                    i32::from(self.current[at - clrs])
                } else {
                    let above = self.vertical[c];
                    self.vertical[c] = above.wrapping_add(diff);
                    above
                };
                if self.row > 0 && column > 0 {
                    let (up, up_left) =
                        (i32::from(self.previous[at]), i32::from(self.previous[at - clrs]));
                    prediction = match jpeg.predictor {
                        1 => prediction,
                        2 => up,
                        3 => up_left,
                        4 => prediction + up - up_left,
                        5 => prediction + ((up - up_left) >> 1),
                        6 => up + ((prediction - up_left) >> 1),
                        7 => (prediction + up) >> 1,
                        _ => 0,
                    };
                }
                let value = prediction.wrapping_add(diff) as u16;
                self.current[at] = value;
                if c <= jpeg.sraw {
                    luma = i32::from(value);
                }
            }
        }
        if self.reader.overrun() {
            return Err(DecodeError::corrupt("CR2 raw JPEG data is truncated"));
        }
        self.row += 1;
        Ok(&self.current)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Writes MSB-first bits with JPEG byte stuffing.
    pub struct Writer {
        pub bytes: Vec<u8>,
        buffer: u32,
        count: u32,
    }

    impl Writer {
        pub fn new() -> Self {
            Self { bytes: Vec::new(), buffer: 0, count: 0 }
        }

        pub fn put(&mut self, value: u32, bits: u32) {
            for i in (0..bits).rev() {
                self.buffer = self.buffer << 1 | (value >> i & 1);
                self.count += 1;
                if self.count == 8 {
                    self.bytes.push(self.buffer as u8);
                    if self.buffer as u8 == 0xFF {
                        self.bytes.push(0);
                    }
                    (self.buffer, self.count) = (0, 0);
                }
            }
        }

        /// One difference with a table of 17 five-bit codes (code = category).
        pub fn difference(&mut self, diff: i32) {
            let size = 32 - diff.unsigned_abs().leading_zeros();
            self.put(size, 5);
            let bits = if diff < 0 { diff - 1 } else { diff } as u32 & ((1 << size) - 1);
            self.put(bits, size);
        }

        pub fn finish(mut self) -> Vec<u8> {
            if self.count > 0 {
                let pad = 8 - self.count;
                self.put((1 << pad) - 1, pad);
            }
            self.bytes.extend([0xFF, 0xD9]);
            self.bytes
        }
    }

    /// SOI, two DHT tables of 5-bit codes, SOF3 and SOS for the given layout.
    pub fn header(bits: u8, width: u16, height: u16, sampling: &[u8]) -> Vec<u8> {
        let mut out = vec![0xFF, SOI, 0xFF, DHT];
        let table: Vec<u8> = [0, 0, 0, 0, 17].iter().copied().chain([0; 11]).chain(0..17).collect();
        out.extend(((2 + 2 * (1 + table.len())) as u16).to_be_bytes());
        for id in 0..2 {
            out.push(id);
            out.extend(&table);
        }
        out.extend([0xFF, SOF3]);
        out.extend(((8 + 3 * sampling.len()) as u16).to_be_bytes());
        out.push(bits);
        out.extend(height.to_be_bytes());
        out.extend(width.to_be_bytes());
        out.push(sampling.len() as u8);
        for (id, &s) in sampling.iter().enumerate() {
            out.extend([id as u8 + 1, s, 0]);
        }
        out.extend([0xFF, SOS]);
        out.extend(((6 + 2 * sampling.len()) as u16).to_be_bytes());
        out.push(sampling.len() as u8);
        for id in 0..sampling.len() {
            out.extend([id as u8 + 1, if id == 0 { 0x00 } else { 0x11 }]);
        }
        out.extend([1, 0, 0]);
        out
    }

    #[test]
    fn predicts_sraw_luma_in_coding_order() {
        // One 4:2:2 MCU row of 2 MCUs: Y1 Y2 Cb Cr each.
        let samples = [[100, 104, 16384, 16380], [110, 90, 16390, 16384]];
        let mut writer = Writer::new();
        let start = 1 << 14;
        let mut luma = 0;
        for (column, mcu) in samples.iter().enumerate() {
            for (c, &value) in mcu.iter().enumerate() {
                let prediction = match (column, c) {
                    (0, 0) => start,
                    (_, 0 | 1) => luma,
                    (0, _) => start,
                    _ => samples[column - 1][c],
                };
                writer.difference(value - prediction);
                if c <= 1 {
                    luma = value;
                }
            }
        }
        let mut bytes = header(15, 4, 1, &[0x21, 0x11, 0x11]);
        bytes.extend(writer.finish());
        let jpeg = LosslessJpeg::parse(&bytes).unwrap();
        assert_eq!((jpeg.samples_per_mcu, jpeg.sraw, jpeg.width), (4, 1, 4));
        let mut rows = jpeg.rows(2);
        let row = rows.next_row().unwrap();
        let expected: Vec<u16> = samples.iter().flatten().map(|&v| v as u16).collect();
        assert_eq!(row, expected.as_slice());
    }
}
