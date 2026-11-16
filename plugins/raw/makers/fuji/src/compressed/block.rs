//! Module: mirante_raw_fuji::compressed::block
//! Purpose: Decodes one column block of Fujifilm's lossless compressed raw data.
//! Created: 2026-11-16
//! Architecture: Six sensor rows at a time are coded as twelve colour lines
//! (3 red, 6 green, 3 blue) predicted from the two lines above. Each sample's
//! context is a quantised gradient selecting adaptive Golomb-Rice statistics.
//! Follows LibRaw's fuji_compressed.cpp (lossless case) step for step.

use super::bits::BitReader;
use mirante_core::DecodeError;

/// Line buffers: two context lines then the lines being decoded, per colour.
const R2: usize = 2;
const G0: usize = 5;
const G2: usize = 7;
const B0: usize = 13;
const B2: usize = 15;
const LINES: usize = 18;
/// (target, source): the last two lines of each colour become the next context.
const CARRY: [(usize, usize); 6] =
    [(0, 3), (1, 4), (G0, G0 + 6), (G0 + 1, G0 + 7), (B0, B0 + 3), (B0 + 1, B0 + 4)];
/// First decoded line and line count of red, green and blue.
const DECODED: [(usize, usize); 3] = [(R2, 3), (G2, 6), (B2, 3)];
/// Gradients are q1 * 9 + q2 with q1, q2 in -4..=4.
const GRADIENTS: usize = 41;
const GRADIENT_WEIGHT: i32 = 9;
const CONTEXTS: usize = 3;
/// Statistics halve when their sample count reaches this.
const MIN_VALUE: i32 = 0x40;
/// Quantiser thresholds for lossless data (q_base 0).
const THRESHOLDS: [i32; 3] = [0x12, 0x43, 0x114];
/// Odd samples need their right-hand even neighbour, so they start this late.
const ODD_DELAY: usize = 8;
pub const ROWS_PER_GROUP: usize = 6;

/// Fixed coding parameters shared by every block of an image.
pub struct Params {
    pub line_width: usize,
    pub xtrans: bool,
    max_value: i32,
    total_values: i32,
    raw_bits: u32,
    /// Zero runs this long or longer escape to a raw `raw_bits` value.
    escape: u32,
    max_diff: i32,
    quantiser: Vec<i8>,
}

impl Params {
    pub fn new(bits: u32, block_width: usize, xtrans: bool) -> Self {
        let max_value = (1 << bits) - 1;
        let total_values = max_value + 1;
        let max_bits = 4 * bits;
        Self {
            line_width: if xtrans { block_width * 2 / 3 } else { block_width / 2 },
            xtrans,
            max_value,
            total_values,
            raw_bits: bits,
            escape: max_bits - bits - 1,
            max_diff: ((total_values + 0x20) >> 6).max(2),
            quantiser: (-max_value..=max_value).map(quantise).collect(),
        }
    }

    fn quantise(&self, difference: i32) -> i32 {
        i32::from(self.quantiser[(difference + self.max_value) as usize])
    }

    /// Index into a colour line for a pixel column of the block.
    pub fn line_index(&self, column: usize) -> usize {
        if self.xtrans {
            let third = column % 3;
            (((column * 2 / 3) & !1) | (third & 1)) + (third >> 1)
        } else {
            column >> 1
        }
    }
}

/// Maps a pixel difference to -4..=4 (LibRaw's setup_qlut with q_base 0).
fn quantise(difference: i32) -> i8 {
    let magnitude = difference.abs();
    let class = if magnitude == 0 {
        0
    } else {
        1 + THRESHOLDS.iter().filter(|&&threshold| magnitude >= threshold).count() as i8
    };
    if difference < 0 { -class } else { class }
}

/// When an even position is interpolated instead of coded.
#[derive(Clone, Copy)]
enum Even {
    Coded,
    Interpolated,
    /// Interpolated where position % 4 equals the value.
    InterpolatedAt(usize),
}

impl Even {
    fn interpolates(self, position: usize) -> bool {
        match self {
            Even::Coded => false,
            Even::Interpolated => true,
            Even::InterpolatedAt(remainder) => position % 4 == remainder,
        }
    }
}

/// Two lines decoded together, in bitstream order. Even positions that no
/// photosite of the line's colour uses are interpolated instead of coded.
type Pass = [(usize, Even); 2];

const XTRANS_PASSES: [Pass; 6] = [
    [(R2, Even::Interpolated), (G2, Even::Coded)],
    [(G2 + 1, Even::Coded), (B2, Even::Interpolated)],
    [(R2 + 1, Even::InterpolatedAt(0)), (G2 + 2, Even::Interpolated)],
    [(G2 + 3, Even::Coded), (B2 + 1, Even::InterpolatedAt(2))],
    [(R2 + 2, Even::InterpolatedAt(2)), (G2 + 4, Even::Coded)],
    [(G2 + 5, Even::Interpolated), (B2 + 2, Even::InterpolatedAt(0))],
];
const BAYER_PASSES: [Pass; 6] = [
    [(R2, Even::Coded), (G2, Even::Coded)],
    [(G2 + 1, Even::Coded), (B2, Even::Coded)],
    [(R2 + 1, Even::Coded), (G2 + 2, Even::Coded)],
    [(G2 + 3, Even::Coded), (B2 + 1, Even::Coded)],
    [(R2 + 2, Even::Coded), (G2 + 4, Even::Coded)],
    [(G2 + 5, Even::Coded), (B2 + 2, Even::Coded)],
];

/// The colour line holding a pixel of colour `color` (0 R, 1 G, 2 B) in `row` 0..6.
pub fn colour_line(color: u8, row: usize) -> usize {
    match color {
        0 => R2 + row / 2,
        2 => B2 + row / 2,
        _ => G2 + row,
    }
}

/// Adaptive Golomb-Rice statistics: sum of magnitudes and sample count.
#[derive(Clone, Copy)]
struct Stat {
    sum: i32,
    count: i32,
}

/// Rice parameter: the smallest shift that brings count up to sum, at most 15.
fn rice_bits(stat: Stat) -> u32 {
    let mut bits = 0;
    if stat.count < stat.sum {
        while bits <= 14 {
            bits += 1;
            if stat.count << bits >= stat.sum {
                break;
            }
        }
    }
    bits
}

fn even_prediction(above: i32, above_left: i32, above_right: i32, above_two: i32) -> i32 {
    let left = (above_left - above).abs();
    let up = (above_two - above).abs();
    let right = (above_right - above).abs();
    let sum = if left > up && left > right {
        above_two + above_right + 2 * above
    } else if right > left && right > up {
        above_two + above_left + 2 * above
    } else {
        above_right + above_left + 2 * above
    };
    sum >> 2
}

struct Decoder<'a, 'p> {
    params: &'p Params,
    bits: BitReader<'a>,
    lines: Vec<u16>,
    stride: usize,
    /// Statistics for [even, odd] positions, per context, per gradient.
    stats: [[[Stat; GRADIENTS]; CONTEXTS]; 2],
}

impl<'a, 'p> Decoder<'a, 'p> {
    fn new(params: &'p Params, bytes: &'a [u8]) -> Self {
        let stride = params.line_width + 2;
        let initial = Stat { sum: params.max_diff, count: 1 };
        Self {
            params,
            bits: BitReader::new(bytes),
            lines: vec![0; LINES * stride],
            stride,
            stats: [[[initial; GRADIENTS]; CONTEXTS]; 2],
        }
    }

    fn at(&self, line: usize, position: usize) -> usize {
        line * self.stride + 1 + position
    }

    fn value(&self, index: usize) -> i32 {
        i32::from(self.lines[index])
    }

    fn interpolate_even(&mut self, at: usize) {
        let s = self.stride;
        let (b, c, d, f) = (
            self.value(at - s),
            self.value(at - s - 1),
            self.value(at - s + 1),
            self.value(at - 2 * s),
        );
        self.lines[at] = even_prediction(b, c, d, f) as u16;
    }

    fn sample_even(&mut self, at: usize, context: usize) -> Result<(), DecodeError> {
        let s = self.stride;
        let (b, c, d, f) = (
            self.value(at - s),
            self.value(at - s - 1),
            self.value(at - s + 1),
            self.value(at - 2 * s),
        );
        let q = |difference| self.params.quantise(difference);
        let gradient = q(b - f) * GRADIENT_WEIGHT + q(c - b);
        self.sample(at, 0, context, gradient, even_prediction(b, c, d, f))
    }

    fn sample_odd(&mut self, at: usize, context: usize) -> Result<(), DecodeError> {
        let s = self.stride;
        let (a, g) = (self.value(at - 1), self.value(at + 1));
        let (b, c, d) = (self.value(at - s), self.value(at - s - 1), self.value(at - s + 1));
        let q = |difference| self.params.quantise(difference);
        let gradient = q(b - c) * GRADIENT_WEIGHT + q(c - a);
        let prediction =
            if (b > c && b > d) || (b < c && b < d) { (g + a + 2 * b) >> 2 } else { (a + g) >> 1 };
        self.sample(at, 1, context, gradient, prediction)
    }

    /// Reads one Golomb-Rice coded residual and stores prediction + residual.
    fn sample(
        &mut self,
        at: usize,
        parity: usize,
        context: usize,
        gradient: i32,
        prediction: i32,
    ) -> Result<(), DecodeError> {
        let params = self.params;
        let stat = &mut self.stats[parity][context][gradient.unsigned_abs() as usize];
        let zeros = self.bits.zeros()?;
        let code = if zeros < params.escape {
            let bits = rice_bits(*stat);
            (i64::from(zeros) << bits) + i64::from(self.bits.read(bits)?)
        } else {
            i64::from(self.bits.read(params.raw_bits)?) + 1
        };
        if code >= i64::from(params.total_values) {
            return Err(DecodeError::corrupt("Fujifilm compressed residual is out of range"));
        }
        let code = code as i32;
        let residual = if code & 1 == 1 { -1 - code / 2 } else { code / 2 };
        stat.sum += residual.abs();
        if stat.count == MIN_VALUE {
            stat.sum >>= 1;
            stat.count >>= 1;
        }
        stat.count += 1;
        let mut value = if gradient < 0 { prediction - residual } else { prediction + residual };
        if value < 0 {
            value += params.total_values;
        } else if value > params.max_value {
            value -= params.total_values;
        }
        self.lines[at] = value.clamp(0, params.max_value) as u16;
        Ok(())
    }

    fn decode_pass(&mut self, pass: &Pass, context: usize) -> Result<(), DecodeError> {
        let width = self.params.line_width;
        let (mut even, mut odd) = (0, 1);
        while even < width || odd < width {
            if even < width {
                for &(line, rule) in pass {
                    let at = self.at(line, even);
                    if rule.interpolates(even) {
                        self.interpolate_even(at);
                    } else {
                        self.sample_even(at, context)?;
                    }
                }
                even += 2;
            }
            if even > ODD_DELAY || even >= width {
                for &(line, _) in pass {
                    self.sample_odd(self.at(line, odd), context)?;
                }
                odd += 2;
            }
        }
        for &(line, _) in pass {
            self.extend_colour(line);
        }
        Ok(())
    }

    /// Fills the padding at both ends of a colour's decoded lines from the line above.
    fn extend_colour(&mut self, line: usize) {
        let &(first, count) = DECODED
            .iter()
            .find(|(first, count)| (*first..first + count).contains(&line))
            .expect("passes only name decoded lines");
        for target in first..first + count {
            self.pad(target);
        }
    }

    fn pad(&mut self, line: usize) {
        let (s, width) = (self.stride, self.params.line_width);
        self.lines[line * s] = self.lines[(line - 1) * s + 1];
        self.lines[line * s + width + 1] = self.lines[(line - 1) * s + width];
    }

    /// Keeps the last two lines of each colour as context and clears the rest.
    fn advance(&mut self) {
        let s = self.stride;
        for (target, source) in CARRY {
            self.lines.copy_within(source * s..(source + 1) * s, target * s);
        }
        for (first, count) in DECODED {
            self.lines[first * s..(first + count) * s].fill(0);
            self.pad(first);
        }
    }
}

/// Decodes `groups` groups of six rows of a block `width` pixels wide.
/// `sources[row * stride + column]` is the line-buffer index of each pixel,
/// where `stride` is the full block width.
pub fn decode_block(
    params: &Params,
    bytes: &[u8],
    sources: &[usize],
    width: usize,
    groups: usize,
) -> Result<Vec<u16>, DecodeError> {
    let full_width = sources.len() / ROWS_PER_GROUP;
    let mut decoder = Decoder::new(params, bytes);
    let passes = if params.xtrans { &XTRANS_PASSES } else { &BAYER_PASSES };
    let mut out = Vec::with_capacity(groups * ROWS_PER_GROUP * width);
    for _ in 0..groups {
        for (index, pass) in passes.iter().enumerate() {
            decoder.decode_pass(pass, index % CONTEXTS)?;
        }
        for row in sources.chunks_exact(full_width) {
            out.extend(row[..width].iter().map(|&source| decoder.lines[source]));
        }
        decoder.advance();
    }
    Ok(out)
}

/// Line-buffer index of every pixel in a block's six rows, for `colour(column, row)`.
pub fn pixel_sources(
    params: &Params,
    width: usize,
    colour: impl Fn(usize, usize) -> u8,
) -> Vec<usize> {
    let stride = params.line_width + 2;
    (0..ROWS_PER_GROUP)
        .flat_map(|row| (0..width).map(move |column| (row, column)))
        .map(|(row, column)| {
            colour_line(colour(column, row), row) * stride + 1 + params.line_index(column)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Packs a string of '0' and '1' into bytes, padding with zeros.
    fn bits(text: &str) -> Vec<u8> {
        let digits: Vec<u8> = text.bytes().map(|b| b - b'0').collect();
        digits
            .chunks(8)
            .map(|c| c.iter().enumerate().fold(0, |acc, (i, &b)| acc | b << (7 - i)))
            .collect()
    }

    #[test]
    fn quantises_gradients() {
        let classes: Vec<i8> =
            [0, 1, 17, 18, 66, 67, 275, 276, 9000, -1, -18, -276].map(quantise).into();
        assert_eq!(classes, [0, 1, 1, 2, 2, 3, 3, 4, 4, -1, -2, -4]);
    }

    #[test]
    fn picks_rice_parameters() {
        let rice = |sum, count| rice_bits(Stat { sum, count });
        assert_eq!(
            [rice(256, 1), rice(256, 3), rice(256, 4), rice(1, 5), rice(1 << 30, 1)],
            [8, 7, 6, 0, 15]
        );
    }

    #[test]
    fn predicts_from_the_smoothest_direction() {
        // above, above-left, above-right, two above
        assert_eq!(even_prediction(100, 100, 100, 100), 100);
        assert_eq!(even_prediction(100, 500, 104, 96), (96 + 104 + 200) >> 2);
        assert_eq!(even_prediction(100, 98, 400, 102), (102 + 98 + 200) >> 2);
    }

    #[test]
    fn decodes_golomb_rice_and_escape_codes() {
        let params = Params::new(12, 12, false);
        // A fresh statistic has sum 64 and count 1, so 6 low bits follow the run.
        let stream = format!("1{:06b}{}1{:012b}", 5, "0".repeat(35), 7);
        let data = bits(&stream);
        let mut decoder = Decoder::new(&params, &data);
        let first = decoder.at(G2, 0);
        decoder.sample_even(first, 0).unwrap();
        // Code 5 is residual -3, wrapped modulo 4096.
        assert_eq!(decoder.lines[first], 4093);
        let stat = decoder.stats[0][0][0];
        assert_eq!((stat.sum, stat.count), (67, 2));
        let second = decoder.at(G2, 2);
        decoder.sample_even(second, 0).unwrap();
        assert_eq!(decoder.lines[second], 4);
        assert!(decoder.sample_even(decoder.at(G2, 4), 0).is_err());
    }

    #[test]
    fn interpolates_and_carries_lines() {
        let params = Params::new(14, 12, true);
        let mut decoder = Decoder::new(&params, &[]);
        let s = decoder.stride;
        decoder.lines[s..2 * s].fill(40);
        decoder.lines[..s].fill(20);
        let at = decoder.at(R2, 2);
        decoder.interpolate_even(at);
        assert_eq!(decoder.lines[at], 40);
        decoder.lines[s + 1 + 1] = 400;
        decoder.interpolate_even(at);
        assert_eq!(decoder.lines[at], (20 + 40 + 80) >> 2);

        decoder.lines[4 * s..5 * s].iter_mut().enumerate().for_each(|(i, v)| *v = i as u16);
        decoder.advance();
        assert_eq!(decoder.lines[s + 3], 3);
        let interior = R2 * s + 1..R2 * s + 1 + params.line_width;
        assert!(decoder.lines[interior].iter().all(|&v| v == 0));
        assert!(decoder.lines[(R2 + 1) * s..(R2 + 3) * s].iter().all(|&v| v == 0));
        // The new line's padding repeats the carried line's end samples.
        assert_eq!(decoder.lines[R2 * s], 1);
        assert_eq!(decoder.lines[R2 * s + params.line_width + 1], params.line_width as u16);
    }

    #[test]
    fn maps_pixels_to_colour_lines() {
        let xtrans = Params::new(14, 12, true);
        let indices: Vec<usize> = (0..7).map(|c| xtrans.line_index(c)).collect();
        assert_eq!(indices, [0, 1, 1, 2, 3, 3, 4]);
        let stride = xtrans.line_width + 2;
        let sources = pixel_sources(&xtrans, 12, |x, y| if (x + y) % 3 == 0 { 0 } else { 1 });
        assert_eq!(sources[0], R2 * stride + 1);
        assert_eq!(sources[12 + 1], G2 * stride + stride + 1 + 1);
        let bayer = Params::new(14, 12, false);
        assert_eq!(bayer.line_index(5), 2);
        let sources = pixel_sources(&bayer, 12, |x, y| [[0, 1], [1, 2]][y % 2][x % 2]);
        assert_eq!(sources[12 + 1], B2 * (bayer.line_width + 2) + 1);
    }

    #[test]
    fn rejects_truncated_blocks() {
        let params = Params::new(14, 12, true);
        let sources = pixel_sources(&params, 12, |x, y| [1, 0, 2][(x + y) % 3]);
        assert!(decode_block(&params, &[0; 8], &sources, 12, 1).is_err());
        // Arbitrary data either decodes to the full size or fails; it never panics.
        if let Ok(values) = decode_block(&params, &[0xA5; 400], &sources, 12, 2) {
            assert_eq!(values.len(), 144);
        }
    }
}
