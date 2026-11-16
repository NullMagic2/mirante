//! Module: mirante_codec::checksum
//! Purpose: CRC-32 (PNG), Adler-32 (zlib) and FNV-1a (stable content keys).
//! Created: 2026-11-16
//! Architecture: Used by PNG chunks and zlib streams; owned by the PNG work.
//! CRC-32 is table-driven with a table built at compile time.

/// Reflected CRC-32 polynomial shared by PNG, zlib's gzip and Ethernet.
const CRC_POLYNOMIAL: u32 = 0xEDB8_8320;
/// Largest prime below 2^16, the Adler-32 modulus.
const ADLER_MODULUS: u32 = 65_521;
/// Bytes that can be summed before Adler-32's u32 sums could overflow (zlib's NMAX).
const ADLER_BLOCK: usize = 5552;

const CRC_TABLE: [u32; 256] = crc_table();

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index = 0;

    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 != 0 { CRC_POLYNOMIAL ^ (value >> 1) } else { value >> 1 };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

fn crc32_update(crc: u32, data: &[u8]) -> u32 {
    data.iter()
        .fold(crc, |crc, &byte| CRC_TABLE[((crc ^ u32::from(byte)) & 0xFF) as usize] ^ (crc >> 8))
}

pub fn crc32(data: &[u8]) -> u32 {
    crc32_parts(&[data])
}

/// CRC-32 over several slices, as PNG computes it over chunk type then data.
pub fn crc32_parts(parts: &[&[u8]]) -> u32 {
    !parts.iter().fold(u32::MAX, |crc, part| crc32_update(crc, part))
}

pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);

    for block in data.chunks(ADLER_BLOCK) {
        for &byte in block {
            a += u32::from(byte);
            b += a;
        }
        a %= ADLER_MODULUS;
        b %= ADLER_MODULUS;
    }
    (b << 16) | a
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// 64-bit FNV-1a over several slices; stable across versions, unlike std hashers.
pub fn fnv1a64(parts: &[&[u8]]) -> u64 {
    parts
        .iter()
        .flat_map(|part| part.iter())
        .fold(FNV_OFFSET, |hash, &byte| (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a64_matches_the_reference_vector() {
        assert_eq!(fnv1a64(&[b"a"]), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(&[b"fo", b"o"]), fnv1a64(&[b"foo"]));
    }

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32_parts(&[b"1234", b"56789"]), 0xCBF4_3926);
    }

    #[test]
    fn adler32_matches_known_values() {
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(adler32(b""), 1);
        // Long input exercises the deferred modulo.
        let long = vec![0xFF; 100_000];
        let (mut a, mut b) = (1u64, 0u64);
        for &byte in &long {
            a = (a + u64::from(byte)) % 65_521;
            b = (b + a) % 65_521;
        }
        assert_eq!(adler32(&long), ((b << 16) | a) as u32);
    }
}
