//! Module: mirante_format_bmp::rle
//! Purpose: Expands BI_RLE8 and BI_RLE4 pixel data into palette indices.
//! Created: 2026-11-16
//! Architecture: Rows come out in stored order (bottom-up for normal files).
//! Pixels the stream skips with delta, end-of-line or end-of-bitmap stay None,
//! which the caller shows as transparent, as browsers do.

const ESCAPE: u8 = 0;
const END_OF_LINE: u8 = 0;
const END_OF_BITMAP: u8 = 1;
const DELTA: u8 = 2;

/// `four_bit` selects RLE4, whose runs alternate the two nibbles of their byte.
/// A stream that ends without an end-of-bitmap marker simply stops.
pub fn expand(data: &[u8], width: usize, height: usize, four_bit: bool) -> Vec<Option<u8>> {
    let mut indices = vec![None; width * height];
    let mut input = data.iter().copied();
    let (mut x, mut y) = (0_usize, 0_usize);
    let nibble = |byte: u8, position: usize| match (four_bit, position % 2) {
        (false, _) => byte,
        (true, 0) => byte >> 4,
        (true, _) => byte & 0x0F,
    };
    let mut put = |x: usize, y: usize, index: u8| {
        if x < width && y < height {
            indices[y * width + x] = Some(index);
        }
    };

    while let (Some(count), Some(value)) = (input.next(), input.next()) {
        if y >= height {
            break;
        }
        if count != ESCAPE {
            for position in 0..usize::from(count) {
                put(x + position, y, nibble(value, position));
            }
            x += usize::from(count);
            continue;
        }
        match value {
            END_OF_LINE => (x, y) = (0, y + 1),
            END_OF_BITMAP => break,
            DELTA => {
                let (Some(dx), Some(dy)) = (input.next(), input.next()) else { break };
                (x, y) = (x + usize::from(dx), y + usize::from(dy));
            }
            absolute => {
                let pixels = usize::from(absolute);
                let bytes = if four_bit { pixels.div_ceil(2) } else { pixels };
                let literal: Vec<u8> = input.by_ref().take(bytes + bytes % 2).collect();
                for position in 0..pixels {
                    let byte =
                        if four_bit { literal.get(position / 2) } else { literal.get(position) };
                    let Some(&byte) = byte else { break };
                    put(x + position, y, nibble(byte, position));
                }
                x += pixels;
            }
        }
    }
    indices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_rle8_runs_literals_and_escapes() {
        // Row 0: run of 2 x 7, literal [1, 2, 3] (padded); end of line;
        // delta (1, 0); run of 1 x 9; end of bitmap.
        let data = [2, 7, 0, 3, 1, 2, 3, 0, 0, 0, 0, 2, 1, 0, 1, 9, 0, 1];
        let indices = expand(&data, 5, 2, false);
        let expected = [7, 7, 1, 2, 3].map(Some).into_iter();
        let second = [None, Some(9), None, None, None].into_iter();
        assert_eq!(indices, expected.chain(second).collect::<Vec<_>>());
    }

    #[test]
    fn expands_rle4_nibbles() {
        // A run of 3 alternating 1 and 2, then a literal of 3 nibbles 4, 5, 6.
        let data = [3, 0x12, 0, 3, 0x45, 0x60, 0, 1];
        let indices = expand(&data, 6, 1, true);
        assert_eq!(indices, [1, 2, 1, 4, 5, 6].map(Some));
    }
}
