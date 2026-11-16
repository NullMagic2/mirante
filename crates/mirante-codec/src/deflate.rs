//! Module: mirante_codec::deflate
//! Purpose: DEFLATE and zlib compression for writers (RFC 1950/1951).
//! Created: 2026-11-16
//! Architecture: Used by the PNG and TIFF encoders; owned by the PNG work.
//! LZ77 with hash chains and one-step lazy matching feeds blocks that are each
//! written stored, fixed or dynamic Huffman, whichever is smallest.

use crate::checksum::adler32;
use crate::inflate::{
    CODE_LENGTH_ORDER, DISTANCE_BASE, DISTANCE_EXTRA, END_OF_BLOCK, LENGTH_BASE, LENGTH_EXTRA,
    MAX_CODE_BITS, fixed_lengths,
};

const WINDOW_SIZE: usize = 32_768;
const WINDOW_MASK: usize = WINDOW_SIZE - 1;
const HASH_BITS: u32 = 15;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
/// Candidates examined per position; trades speed for ratio.
const MAX_CHAIN: usize = 64;
/// A match this long is taken without searching further or trying the next position.
const NICE_MATCH: usize = 128;
const BLOCK_TOKENS: usize = 16_384;
const MAX_STORED: usize = 65_535;
const MAX_CODE_LENGTH_BITS: usize = 7;
const LITERAL_SYMBOLS: usize = 286;
const DISTANCE_SYMBOLS: usize = 30;
const NO_POSITION: usize = usize::MAX;
/// zlib header: 32 KiB window, DEFLATE, default level; a multiple of 31.
const ZLIB_HEADER: [u8; 2] = [0x78, 0x9C];

/// A literal (`length == 0`, `value` is the byte) or a match (`value` is the distance).
#[derive(Clone, Copy)]
struct Token {
    length: u16,
    value: u16,
}

const LENGTH_SYMBOL: [u8; MAX_MATCH + 1] = length_symbols();

const fn length_symbols() -> [u8; MAX_MATCH + 1] {
    let mut table = [0u8; MAX_MATCH + 1];
    let mut index = 0;
    // Ascending order lets code 285 claim length 258 from code 284's range.
    while index < LENGTH_BASE.len() {
        let base = LENGTH_BASE[index] as usize;
        let mut length = base;
        while length < base + (1 << LENGTH_EXTRA[index]) && length <= MAX_MATCH {
            table[length] = index as u8;
            length += 1;
        }
        index += 1;
    }
    table
}

fn distance_symbol(distance: u16) -> usize {
    DISTANCE_BASE.partition_point(|&base| base <= distance) - 1
}

struct BitWriter {
    output: Vec<u8>,
    buffer: u64,
    count: u32,
}

impl BitWriter {
    /// Appends `bits` (at most 32) least significant bit first.
    fn write(&mut self, value: u32, bits: u32) {
        self.buffer |= u64::from(value) << self.count;
        self.count += bits;
        if self.count >= 32 {
            self.output.extend_from_slice(&(self.buffer as u32).to_le_bytes());
            self.buffer >>= 32;
            self.count -= 32;
        }
    }

    fn align_to_byte(&mut self) {
        while self.count > 0 {
            self.output.push(self.buffer as u8);
            self.buffer >>= 8;
            self.count = self.count.saturating_sub(8);
        }
    }
}

/// Finds back-references with hash chains over a 32 KiB window.
struct Matcher<'a> {
    data: &'a [u8],
    head: Vec<usize>,
    previous: Vec<usize>,
}

impl<'a> Matcher<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            head: vec![NO_POSITION; 1 << HASH_BITS],
            previous: vec![NO_POSITION; WINDOW_SIZE],
        }
    }

    fn hash(&self, position: usize) -> usize {
        let bytes = &self.data[position..position + MIN_MATCH];
        let key = u32::from(bytes[0]) | u32::from(bytes[1]) << 8 | u32::from(bytes[2]) << 16;
        (key.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
    }

    fn insert(&mut self, position: usize) {
        if position + MIN_MATCH <= self.data.len() {
            let hash = self.hash(position);
            self.previous[position & WINDOW_MASK] = self.head[hash];
            self.head[hash] = position;
        }
    }

    /// Longest match at `position` that beats `at_least`, as (length, distance).
    fn find(&self, position: usize, at_least: usize) -> Option<(usize, usize)> {
        if position + MIN_MATCH > self.data.len() {
            return None;
        }
        let current = &self.data[position..self.data.len().min(position + MAX_MATCH)];
        let mut best = (at_least.max(MIN_MATCH - 1), 0);
        let mut candidate = self.head[self.hash(position)];

        for _ in 0..MAX_CHAIN {
            if candidate == NO_POSITION || position - candidate > WINDOW_SIZE {
                break;
            }
            let earlier = &self.data[candidate..];
            if earlier.get(best.0) == current.get(best.0) {
                let length = current.iter().zip(earlier).take_while(|(a, b)| a == b).count();
                if length > best.0 {
                    best = (length, position - candidate);
                    if length >= NICE_MATCH || length == current.len() {
                        break;
                    }
                }
            }
            // Chains only go backwards; anything else is a slot reused by a newer position.
            let next = self.previous[candidate & WINDOW_MASK];
            if next >= candidate {
                break;
            }
            candidate = next;
        }
        (best.1 != 0).then_some(best)
    }
}

/// Code lengths for `frequencies`, no longer than `max_bits`; unused symbols get 0.
fn huffman_lengths(frequencies: &[u32], max_bits: usize) -> Vec<u8> {
    let mut lengths = vec![0u8; frequencies.len()];
    let mut used: Vec<(u32, usize)> = frequencies
        .iter()
        .enumerate()
        .filter(|&(_, &frequency)| frequency > 0)
        .map(|(symbol, &frequency)| (frequency, symbol))
        .collect();
    if used.len() <= 1 {
        used.iter().for_each(|&(_, symbol)| lengths[symbol] = 1);
        return lengths;
    }
    used.sort_unstable();

    // Two-queue Huffman construction: leaves are sorted, merged nodes come out sorted.
    let leaves = used.len();
    let mut weights: Vec<u64> = used.iter().map(|&(frequency, _)| u64::from(frequency)).collect();
    let mut parents = vec![0usize; 2 * leaves - 1];
    let (mut next_leaf, mut next_node) = (0, leaves);
    while weights.len() < 2 * leaves - 1 {
        let mut pick = || {
            let take_leaf = next_leaf < leaves
                && (next_node >= weights.len() || weights[next_leaf] <= weights[next_node]);
            let index = if take_leaf { &mut next_leaf } else { &mut next_node };
            *index += 1;
            *index - 1
        };
        let (first, second) = (pick(), pick());
        parents[first] = weights.len();
        parents[second] = weights.len();
        weights.push(weights[first] + weights[second]);
    }
    let mut depths = vec![0usize; weights.len()];
    for node in (0..weights.len() - 1).rev() {
        depths[node] = depths[parents[node]] + 1;
    }

    // Clamp to max_bits, then restore the Kraft equality by lengthening short codes.
    let mut counts = vec![0u32; max_bits + 1];
    depths[..leaves].iter().for_each(|&depth| counts[depth.min(max_bits)] += 1);
    let mut total: u32 = (1..=max_bits).map(|bits| counts[bits] << (max_bits - bits)).sum();
    while total > 1 << max_bits {
        counts[max_bits] -= 1;
        if let Some(bits) = (1..max_bits).rev().find(|&bits| counts[bits] > 0) {
            counts[bits] -= 1;
            counts[bits + 1] += 2;
        }
        total -= 1;
    }

    // The most frequent symbols take the shortest codes.
    let mut remaining = leaves;
    for (bits, &count) in counts.iter().enumerate().skip(1) {
        for _ in 0..count {
            remaining -= 1;
            lengths[used[remaining].1] = bits as u8;
        }
    }
    lengths
}

/// Canonical codes, bit-reversed for least-significant-first output.
fn canonical_codes(lengths: &[u8]) -> Vec<u16> {
    let mut counts = [0u16; MAX_CODE_BITS + 1];
    lengths.iter().for_each(|&length| counts[usize::from(length)] += 1);
    counts[0] = 0;
    let mut next = [0u16; MAX_CODE_BITS + 2];
    for bits in 1..=MAX_CODE_BITS {
        next[bits + 1] = (next[bits] + counts[bits]) << 1;
    }
    lengths
        .iter()
        .map(|&length| {
            if length == 0 {
                return 0;
            }
            let code = next[usize::from(length)];
            next[usize::from(length)] += 1;
            code.reverse_bits() >> (16 - length)
        })
        .collect()
}

/// Run-length codes a dynamic block's code lengths as (symbol, extra bits value).
fn code_length_runs(lengths: &[u8]) -> Vec<(u8, u8)> {
    let mut runs = Vec::new();
    let mut index = 0;
    while index < lengths.len() {
        let value = lengths[index];
        let mut run = lengths[index..].iter().take_while(|&&length| length == value).count();
        index += run;
        if value == 0 {
            while run >= 11 {
                let chunk = run.min(138);
                runs.push((18, (chunk - 11) as u8));
                run -= chunk;
            }
            if run >= 3 {
                runs.push((17, (run - 3) as u8));
                run = 0;
            }
        } else {
            runs.push((value, 0));
            run -= 1;
            while run >= 3 {
                let chunk = run.min(6);
                runs.push((16, (chunk - 3) as u8));
                run -= chunk;
            }
        }
        runs.extend(std::iter::repeat_n((value, 0), run));
    }
    runs
}

fn code_length_extra_bits(symbol: u8) -> u32 {
    match symbol {
        16 => 2,
        17 => 3,
        18 => 7,
        _ => 0,
    }
}

/// Everything needed to write one block with dynamic Huffman codes.
struct DynamicCodes {
    literal: Vec<u8>,
    distance: Vec<u8>,
    code_length: Vec<u8>,
    runs: Vec<(u8, u8)>,
    code_length_count: usize,
}

impl DynamicCodes {
    fn new(literal_frequencies: &[u32], distance_frequencies: &[u32]) -> Self {
        let literal = huffman_lengths(literal_frequencies, MAX_CODE_BITS);
        let mut distance = huffman_lengths(distance_frequencies, MAX_CODE_BITS);
        // Some inflaters reject a block that declares no distance code at all.
        if distance.iter().all(|&length| length == 0) {
            distance[0] = 1;
        }
        let literal_count =
            257.max(literal.iter().rposition(|&length| length > 0).unwrap_or(0) + 1);
        let distance_count = distance.iter().rposition(|&length| length > 0).unwrap_or(0) + 1;
        let all: Vec<u8> = [&literal[..literal_count], &distance[..distance_count]].concat();
        let runs = code_length_runs(&all);

        let mut frequencies = [0u32; 19];
        runs.iter().for_each(|&(symbol, _)| frequencies[usize::from(symbol)] += 1);
        // zlib rejects a code-length code with a single symbol, so add a spare one.
        if frequencies.iter().filter(|&&frequency| frequency > 0).count() < 2 {
            frequencies[usize::from(frequencies[0] != 0)] = 1;
        }
        let code_length = huffman_lengths(&frequencies, MAX_CODE_LENGTH_BITS);
        let code_length_count = 4.max(
            CODE_LENGTH_ORDER.iter().rposition(|&symbol| code_length[symbol] > 0).unwrap_or(0) + 1,
        );
        Self {
            literal: literal[..literal_count].to_vec(),
            distance: distance[..distance_count].to_vec(),
            code_length,
            runs,
            code_length_count,
        }
    }

    fn header_bits(&self) -> u64 {
        let runs: u64 = self
            .runs
            .iter()
            .map(|&(symbol, _)| {
                u64::from(self.code_length[usize::from(symbol)])
                    + u64::from(code_length_extra_bits(symbol))
            })
            .sum();
        14 + 3 * self.code_length_count as u64 + runs
    }

    fn write_header(&self, writer: &mut BitWriter) {
        writer.write(self.literal.len() as u32 - 257, 5);
        writer.write(self.distance.len() as u32 - 1, 5);
        writer.write(self.code_length_count as u32 - 4, 4);
        for &symbol in &CODE_LENGTH_ORDER[..self.code_length_count] {
            writer.write(u32::from(self.code_length[symbol]), 3);
        }
        let codes = canonical_codes(&self.code_length);
        for &(symbol, extra) in &self.runs {
            let symbol = usize::from(symbol);
            writer.write(u32::from(codes[symbol]), u32::from(self.code_length[symbol]));
            writer.write(u32::from(extra), code_length_extra_bits(symbol as u8));
        }
    }
}

/// Bits the tokens take with the given code lengths, extra bits included.
fn token_bits(
    literal_frequencies: &[u32],
    distance_frequencies: &[u32],
    literal: &[u8],
    distance: &[u8],
) -> u64 {
    let literal_bits: u64 = literal_frequencies
        .iter()
        .enumerate()
        .map(|(symbol, &frequency)| {
            let extra = symbol.checked_sub(257).map_or(0, |index| LENGTH_EXTRA[index]);
            u64::from(frequency) * u64::from(literal.get(symbol).copied().unwrap_or(0) + extra)
        })
        .sum();
    let distance_bits: u64 = distance_frequencies
        .iter()
        .enumerate()
        .map(|(symbol, &frequency)| {
            let bits = distance.get(symbol).copied().unwrap_or(0) + DISTANCE_EXTRA[symbol];
            u64::from(frequency) * u64::from(bits)
        })
        .sum();
    literal_bits + distance_bits
}

fn write_tokens(writer: &mut BitWriter, tokens: &[Token], literal: &[u8], distance: &[u8]) {
    let literal_codes = canonical_codes(literal);
    let distance_codes = canonical_codes(distance);
    let write_literal = |writer: &mut BitWriter, symbol: usize| {
        writer.write(u32::from(literal_codes[symbol]), u32::from(literal[symbol]));
    };

    for token in tokens {
        if token.length == 0 {
            write_literal(writer, usize::from(token.value));
            continue;
        }
        let length_index = usize::from(LENGTH_SYMBOL[usize::from(token.length)]);
        write_literal(writer, 257 + length_index);
        let extra = token.length - LENGTH_BASE[length_index];
        writer.write(u32::from(extra), u32::from(LENGTH_EXTRA[length_index]));

        let index = distance_symbol(token.value);
        writer.write(u32::from(distance_codes[index]), u32::from(distance[index]));
        let extra = token.value - DISTANCE_BASE[index];
        writer.write(u32::from(extra), u32::from(DISTANCE_EXTRA[index]));
    }
    write_literal(writer, usize::from(END_OF_BLOCK));
}

/// Writes one block in whichever of the three encodings is smallest.
fn write_block(writer: &mut BitWriter, tokens: &[Token], raw: &[u8], last: bool) {
    let mut literal_frequencies = [0u32; LITERAL_SYMBOLS];
    let mut distance_frequencies = [0u32; DISTANCE_SYMBOLS];
    for token in tokens {
        if token.length == 0 {
            literal_frequencies[usize::from(token.value)] += 1;
        } else {
            literal_frequencies[257 + usize::from(LENGTH_SYMBOL[usize::from(token.length)])] += 1;
            distance_frequencies[distance_symbol(token.value)] += 1;
        }
    }
    literal_frequencies[usize::from(END_OF_BLOCK)] = 1;

    let dynamic = DynamicCodes::new(&literal_frequencies, &distance_frequencies);
    let dynamic_bits = dynamic.header_bits()
        + token_bits(
            &literal_frequencies,
            &distance_frequencies,
            &dynamic.literal,
            &dynamic.distance,
        );
    let (fixed_literal, fixed_distance) = fixed_lengths();
    let fixed_bits =
        token_bits(&literal_frequencies, &distance_frequencies, &fixed_literal, &fixed_distance);
    let stored_bits = (raw.len().div_ceil(MAX_STORED).max(1) * 5 + raw.len()) as u64 * 8;

    if stored_bits <= dynamic_bits.min(fixed_bits) {
        let chunks: Vec<&[u8]> =
            if raw.is_empty() { vec![raw] } else { raw.chunks(MAX_STORED).collect() };
        for (index, chunk) in chunks.iter().enumerate() {
            writer.write(u32::from(last && index + 1 == chunks.len()), 3);
            writer.align_to_byte();
            let length = chunk.len() as u16;
            writer.output.extend_from_slice(&length.to_le_bytes());
            writer.output.extend_from_slice(&(!length).to_le_bytes());
            writer.output.extend_from_slice(chunk);
        }
    } else if fixed_bits <= dynamic_bits {
        writer.write(u32::from(last) | 1 << 1, 3);
        write_tokens(writer, tokens, &fixed_literal, &fixed_distance);
    } else {
        writer.write(u32::from(last) | 2 << 1, 3);
        dynamic.write_header(writer);
        write_tokens(writer, tokens, &dynamic.literal, &dynamic.distance);
    }
}

/// Compresses into a raw DEFLATE stream.
pub fn deflate(data: &[u8]) -> Vec<u8> {
    let mut writer =
        BitWriter { output: Vec::with_capacity(data.len() / 2 + 64), buffer: 0, count: 0 };
    let mut matcher = Matcher::new(data);
    let mut tokens = Vec::with_capacity(BLOCK_TOKENS + 2);
    let (mut position, mut block_start) = (0, 0);
    // A match found one byte back, held while the current position may do better.
    let mut pending: Option<(usize, usize)> = None;

    while position < data.len() {
        if pending.is_none() && tokens.len() >= BLOCK_TOKENS {
            write_block(&mut writer, &tokens, &data[block_start..position], false);
            tokens.clear();
            block_start = position;
        }
        let found = matcher.find(position, pending.map_or(0, |(length, _)| length));
        matcher.insert(position);

        let (length, distance, start) = match (pending, found) {
            (Some(_), Some(better)) => {
                tokens.push(Token { length: 0, value: u16::from(data[position - 1]) });
                pending = Some(better);
                position += 1;
                continue;
            }
            (Some((length, distance)), None) => (length, distance, position - 1),
            (None, Some((length, distance))) if length >= NICE_MATCH => {
                (length, distance, position)
            }
            (None, Some(found)) => {
                pending = Some(found);
                position += 1;
                continue;
            }
            (None, None) => {
                tokens.push(Token { length: 0, value: u16::from(data[position]) });
                position += 1;
                continue;
            }
        };
        tokens.push(Token { length: length as u16, value: distance as u16 });
        pending = None;
        for inside in position + 1..start + length {
            matcher.insert(inside);
        }
        position = start + length;
    }
    if let Some((length, distance)) = pending {
        tokens.push(Token { length: length as u16, value: distance as u16 });
    }
    write_block(&mut writer, &tokens, &data[block_start..], true);
    writer.align_to_byte();
    writer.output
}

/// Compresses into a zlib stream (header, DEFLATE data, Adler-32).
pub fn zlib_compress(data: &[u8]) -> Vec<u8> {
    let mut output = ZLIB_HEADER.to_vec();
    output.extend_from_slice(&deflate(data));
    output.extend_from_slice(&adler32(data).to_be_bytes());
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inflate::{inflate, zlib_decompress};

    fn round_trip(data: &[u8]) -> Vec<u8> {
        let compressed = zlib_compress(data);
        assert_eq!(zlib_decompress(&compressed, data.len()).unwrap(), data);
        compressed
    }

    #[test]
    fn round_trips_edge_cases() {
        round_trip(b"");
        round_trip(b"a");
        round_trip(b"abcabcabcabcabcabcabc");
        assert!(round_trip(&[0u8; 100_000]).len() < 300);
    }

    #[test]
    fn round_trips_mixed_data_through_every_block_type() {
        let mut state = 7u32;
        let mut data = Vec::new();
        for round in 0..40u32 {
            for _ in 0..5_000 {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                // Alternate noise (stored), skewed text (dynamic) and runs.
                data.push(match round % 3 {
                    0 => (state >> 24) as u8,
                    1 => b"eeeeettaoinshrdlu"[(state >> 24) as usize % 17],
                    _ => (round * 13) as u8,
                });
            }
        }
        let compressed = round_trip(&data);
        // zlib -9 reaches 111 831 bytes on this input.
        assert!(compressed.len() < 113_000);
    }

    #[test]
    fn limits_match_distance_to_the_window() {
        // Noise whose only repeat sits just past the window, so it cannot be referenced.
        let mut state = 1u32;
        let mut data: Vec<u8> = (0..WINDOW_SIZE + 10)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 24) as u8
            })
            .collect();
        data.extend_from_within(..300);
        assert_eq!(inflate(&deflate(&data), data.len()).unwrap(), data);
    }

    #[test]
    fn inflates_dynamic_headers_at_every_bit_offset() {
        // A fixed block of nine-bit literals shifts the dynamic header through all
        // alignments; 19 code-length codes need more bits than one refill holds.
        let fixed_codes = canonical_codes(&fixed_lengths().0);
        let mut literal = vec![9u8; 257];
        literal[usize::from(END_OF_BLOCK)] = 1;
        let literal_codes = canonical_codes(&literal);
        let code_length_codes = canonical_codes(&[5; 19]);

        for shift in 0..8 {
            let mut writer = BitWriter { output: Vec::new(), buffer: 0, count: 0 };
            writer.write(1 << 1, 3);
            (0..shift).for_each(|_| writer.write(u32::from(fixed_codes[200]), 9));
            writer.write(u32::from(fixed_codes[256]), 7);
            writer.write(1 | 2 << 1, 3);
            writer.write(0, 5);
            writer.write(0, 5);
            writer.write(15, 4);
            (0..19).for_each(|_| writer.write(5, 3));
            for &length in literal.iter().chain(&[1]) {
                writer.write(u32::from(code_length_codes[usize::from(length)]), 5);
            }
            writer.write(u32::from(literal_codes[200]), 9);
            writer.write(u32::from(literal_codes[256]), 1);
            writer.align_to_byte();
            assert_eq!(inflate(&writer.output, 64).unwrap(), vec![200; shift + 1]);
        }
    }

    #[test]
    fn length_limited_codes_stay_valid() {
        // Fibonacci frequencies force an unlimited Huffman tree deeper than 15 bits.
        let mut frequencies = vec![1u32, 1];
        while frequencies.len() < 25 {
            frequencies
                .push(frequencies[frequencies.len() - 1] + frequencies[frequencies.len() - 2]);
        }
        let lengths = huffman_lengths(&frequencies, MAX_CODE_BITS);
        let kraft: u32 =
            lengths.iter().map(|&length| 1 << (MAX_CODE_BITS - usize::from(length))).sum();
        assert_eq!(kraft, 1 << MAX_CODE_BITS);
        assert!(lengths.iter().all(|&length| (1..=15).contains(&length)));
    }
}
