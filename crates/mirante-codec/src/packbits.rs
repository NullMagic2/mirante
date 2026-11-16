//! Module: mirante_codec::packbits
//! Purpose: PackBits run-length decoding (TIFF compression 32773, PSD RLE).
//! Created: 2026-11-16
//! Architecture: Shared by the TIFF and PSD plugins; owned by the TIFF/BMP/PSD work.

use mirante_core::DecodeError;

/// The most output one input byte can produce: a two-byte run of 128.
const MAX_EXPANSION: usize = 64;

/// Expands PackBits data until `expected` bytes are produced. A run that
/// overshoots is cut; input that ends early is an error.
pub fn unpack(data: &[u8], expected: usize) -> Result<Vec<u8>, DecodeError> {
    let mut output = Vec::with_capacity(expected.min(data.len().saturating_mul(MAX_EXPANSION)));
    let mut input = data.iter().copied();

    while output.len() < expected {
        let header = input.next().ok_or_else(truncated)? as i8;
        match header {
            0..=127 => {
                for _ in 0..=header {
                    output.push(input.next().ok_or_else(truncated)?);
                }
            }
            // -128 is a no-op by definition.
            -128 => {}
            _ => {
                let value = input.next().ok_or_else(truncated)?;
                let length = 1 - isize::from(header);
                output.extend(std::iter::repeat_n(value, length as usize));
            }
        }
    }
    output.truncate(expected);
    Ok(output)
}

fn truncated() -> DecodeError {
    DecodeError::corrupt("PackBits data ends early")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_literals_and_runs() {
        // The example from the TIFF 6.0 specification.
        let packed = [
            0xFE, 0xAA, 0x02, 0x80, 0x00, 0x2A, 0xFD, 0xAA, 0x03, 0x80, 0x00, 0x2A, 0x22, 0xF7,
            0xAA,
        ];
        let expected = [
            0xAA, 0xAA, 0xAA, 0x80, 0x00, 0x2A, 0xAA, 0xAA, 0xAA, 0xAA, 0x80, 0x00, 0x2A, 0x22,
            0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA,
        ];
        assert_eq!(unpack(&packed, expected.len()).unwrap(), expected);
    }

    #[test]
    fn short_input_is_an_error() {
        assert!(unpack(&[0x05, 1, 2], 6).is_err());
    }
}
