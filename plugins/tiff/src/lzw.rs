//! Module: mirante_format_tiff::lzw
//! Purpose: TIFF LZW decompression, including the pre-6.0 "old-style" variant.
//! Created: 2026-11-16
//! Architecture: Codes are 9 to 12 bits. New-style streams are MSB-first and widen
//! one code early; old-style streams are LSB-first and widen on time.

use mirante_core::DecodeError;

const CLEAR: u16 = 256;
const END: u16 = 257;
const FIRST_FREE: usize = 258;
const MIN_WIDTH: u32 = 9;
const MAX_WIDTH: u32 = 12;
const TABLE_SIZE: usize = 1 << MAX_WIDTH;

struct BitReader<'a> {
    data: &'a [u8],
    position: usize,
    buffer: u32,
    count: u32,
    lsb_first: bool,
}

impl BitReader<'_> {
    fn code(&mut self, width: u32) -> Option<u16> {
        while self.count < width {
            let byte = u32::from(*self.data.get(self.position)?);
            self.position += 1;
            if self.lsb_first {
                self.buffer |= byte << self.count;
            } else {
                self.buffer = (self.buffer << 8) | byte;
            }
            self.count += 8;
        }
        self.count -= width;
        let mask = (1 << width) - 1;
        let code = if self.lsb_first {
            let code = self.buffer & mask;
            self.buffer >>= width;
            code
        } else {
            (self.buffer >> self.count) & mask
        };
        Some(code as u16)
    }
}

/// One dictionary string: the code it extends, its last byte and its length.
#[derive(Clone, Copy, Default)]
struct Entry {
    prefix: u16,
    last: u8,
    first: u8,
    length: u32,
}

/// Decompresses up to `limit` bytes; a stream without an end code stops at the
/// end of its data, and the caller checks that enough rows arrived.
pub fn decompress(data: &[u8], limit: usize) -> Result<Vec<u8>, DecodeError> {
    // libtiff's test: old-style streams start with an LSB-first clear code.
    let old_style = data.len() >= 2 && data[0] == 0 && data[1] & 1 == 1;
    let early_change = usize::from(!old_style);
    let mut bits = BitReader { data, position: 0, buffer: 0, count: 0, lsb_first: old_style };
    let mut table = vec![Entry::default(); TABLE_SIZE];
    for (byte, entry) in table.iter_mut().take(256).enumerate() {
        *entry = Entry { prefix: 0, last: byte as u8, first: byte as u8, length: 1 };
    }
    let mut output = Vec::new();
    let mut next = FIRST_FREE;
    let mut width = MIN_WIDTH;
    let mut previous: Option<u16> = None;

    while output.len() < limit {
        let Some(code) = bits.code(width) else { break };
        if code == CLEAR {
            (next, width, previous) = (FIRST_FREE, MIN_WIDTH, None);
            continue;
        }
        if code == END {
            break;
        }
        let code_index = usize::from(code);
        let entry = match previous {
            None if code_index < 256 => table[code_index],
            None => return Err(corrupt()),
            Some(previous) => {
                let prior = table[usize::from(previous)];
                let first = if code_index < next { table[code_index].first } else { prior.first };
                if code_index > next {
                    return Err(corrupt());
                }
                if next < TABLE_SIZE {
                    let added = Entry {
                        prefix: previous,
                        last: first,
                        first: prior.first,
                        length: prior.length + 1,
                    };
                    table[next] = added;
                    next += 1;
                    if next + early_change >= 1 << width && width < MAX_WIDTH {
                        width += 1;
                    }
                }
                table[code_index]
            }
        };
        write_string(&table, entry, &mut output);
        previous = Some(code);
    }
    output.truncate(limit);
    Ok(output)
}

/// Appends an entry's string by walking its prefix chain backwards.
fn write_string(table: &[Entry], entry: Entry, output: &mut Vec<u8>) {
    let start = output.len();
    output.resize(start + entry.length as usize, 0);
    let mut current = entry;
    for slot in output[start..].iter_mut().rev() {
        *slot = current.last;
        current = table[usize::from(current.prefix)];
    }
}

fn corrupt() -> DecodeError {
    DecodeError::corrupt("invalid TIFF LZW code")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A plain LZW encoder (no dictionary reuse) is enough to exercise the decoder:
    /// it writes literal codes only, widening exactly as a new-style decoder expects.
    pub(crate) fn compress_literals(data: &[u8]) -> Vec<u8> {
        let mut codes = vec![CLEAR];
        codes.extend(data.iter().map(|&b| u16::from(b)));
        codes.push(END);
        let (mut output, mut buffer, mut count) = (Vec::new(), 0_u32, 0);
        let mut next = FIRST_FREE;
        let mut width = MIN_WIDTH;
        for (index, &code) in codes.iter().enumerate() {
            buffer = (buffer << width) | u32::from(code);
            count += width;
            while count >= 8 {
                count -= 8;
                output.push((buffer >> count) as u8);
            }
            // The decoder adds one entry per code after the first literal.
            if index >= 2 && code != END {
                next += 1;
                if next + 1 >= 1 << width {
                    width += 1;
                }
            }
        }
        if count > 0 {
            output.push((buffer << (8 - count)) as u8);
        }
        output
    }

    #[test]
    fn decodes_repeated_strings() {
        // Codes: clear, 'a', 'b', 258 ("ab"), 260 ("aba", the KwKwK case), end.
        let codes = [256_u16, 97, 98, 258, 260, 257];
        let mut bits = String::new();
        for code in codes {
            bits.push_str(&format!("{code:09b}"));
        }
        while bits.len() % 8 != 0 {
            bits.push('0');
        }
        let bytes: Vec<u8> = bits
            .as_bytes()
            .chunks(8)
            .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 2).unwrap())
            .collect();
        assert_eq!(decompress(&bytes, 100).unwrap(), b"abababa");
    }

    #[test]
    fn literal_stream_round_trips_across_width_changes() {
        let data: Vec<u8> = (0..2000_u32).map(|i| (i * 7 % 251) as u8).collect();
        assert_eq!(decompress(&compress_literals(&data), data.len()).unwrap(), data);
    }
}
