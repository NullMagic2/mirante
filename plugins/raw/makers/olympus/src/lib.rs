//! Module: mirante_raw_olympus
//! Purpose: Decodes Olympus RAW files (ORF) into undeveloped photosites.
//! Created: 2026-11-16
//! Architecture: ORF is TIFF with its own magic and the raw image in IFD0.
//! compressed.rs decodes the 12-bit lossless format; maker_note.rs reads levels
//! and crop. Format choice ported from dcraw by Dave Coffin. Returns a NativeRaw
//! (see mirante_raw::frame); the RAW plugin develops it.

mod compressed;
mod maker_note;

use mirante_codec::tiff::{Ifd, Tiff, tag};
use mirante_core::DecodeError;
use mirante_raw::demosaic::CfaPattern;
use mirante_raw::unpack::{PackedBits, RawPlane};
use mirante_raw::{Area, BlackLevel, NativeRaw, tiff_raw};

use maker_note::MakerNote;

const UNCOMPRESSED: u32 = 1;
const OUTPUT_BITS: u32 = 12;
const EXIF_CFA_PATTERN: u16 = 0xa302;
/// Packed 12-bit data is read in little-endian 32-bit words, high bits first.
const PACKED_WORD_BYTES: usize = 4;

pub fn decode(bytes: &[u8]) -> Result<NativeRaw, DecodeError> {
    let tiff = Tiff::parse(bytes)?;
    let main = tiff.ifds.first().ok_or_else(|| DecodeError::corrupt("ORF has no IFD"))?;
    let plane = read_plane(&tiff, main)?;
    let note = MakerNote::read(&tiff)?;
    let pattern = cfa_pattern(&tiff, main)?;
    let full = Area { left: 0, top: 0, width: plane.width, height: plane.height };
    let visible = note.crop.unwrap_or(full);
    let fits = visible.width > 0
        && visible.height > 0
        && visible.width <= plane.width.saturating_sub(visible.left)
        && visible.height <= plane.height.saturating_sub(visible.top);
    if !fits {
        return Err(DecodeError::corrupt("ORF crop lies outside the raw image"));
    }
    Ok(NativeRaw {
        visible,
        black: BlackLevel::bayer(&pattern, note.black.unwrap_or([0.0; 4])),
        white: ((1 << OUTPUT_BITS) - 1) as f32,
        white_balance: note.red_blue.map(|[red, blue]| [red, 1.0, blue]),
        pattern: Some(pattern),
        plane,
        make: make_name(&tiff_raw::text(&tiff, main, tag::MAKE)?),
        model: tiff_raw::text(&tiff, main, tag::MODEL)?,
        orientation: tiff_raw::orientation(&tiff)?,
    })
}

fn read_plane(tiff: &Tiff<'_>, ifd: &Ifd) -> Result<RawPlane, DecodeError> {
    let width = tiff.get_u32(ifd, tag::IMAGE_WIDTH)?.unwrap_or(0);
    let height = tiff.get_u32(ifd, tag::IMAGE_LENGTH)?.unwrap_or(0);
    let pixels = DecodeError::check_size(width, height)?;
    let (width, height) = (width as usize, height as usize);
    let bits = tiff.get_u32(ifd, tag::BITS_PER_SAMPLE)?.unwrap_or(0);
    let compression = tiff.get_u32(ifd, tag::COMPRESSION)?.unwrap_or(UNCOMPRESSED);
    if compression != UNCOMPRESSED {
        return Err(DecodeError::unsupported(format!("ORF compression {compression}")));
    }
    let data = tiff_raw::strip_data(tiff, ifd)?;
    // dcraw's rules: 1.5 bytes per photosite is packed, a 16-bit header with
    // more than 1/7 byte per photosite is Olympus compression.
    let samples = if data.len() * 2 == pixels * 3 && pixels % 8 == 0 {
        unpack_twelve_bits(data, pixels)
    } else if bits == 16 && data.len() * 7 > pixels {
        compressed::decode(data, width, height)?
    } else {
        return Err(DecodeError::unsupported(format!("ORF layout with {bits}-bit samples")));
    };
    Ok(RawPlane { width, height, samples_per_pixel: 1, bits_per_sample: OUTPUT_BITS, samples })
}

/// dcraw's packed_load_raw with load_flags 24: swap each 32-bit word to big-endian
/// order and the stream becomes plain MSB-first 12-bit samples.
fn unpack_twelve_bits(data: &[u8], pixels: usize) -> Vec<u16> {
    let swapped: Vec<u8> = data
        .chunks_exact(PACKED_WORD_BYTES)
        .flat_map(|word| [word[3], word[2], word[1], word[0]])
        .collect();
    PackedBits::new(&swapped, OUTPUT_BITS).take(pixels).collect()
}

/// EXIF CFAPattern (2x2 only, as dcraw); RGGB when absent.
fn cfa_pattern(tiff: &Tiff<'_>, main: &Ifd) -> Result<CfaPattern, DecodeError> {
    let mut colors = vec![0, 1, 1, 2];
    for exif in tiff.child_ifds(main, tag::EXIF_IFD)? {
        if let Some(entry) = exif.get(EXIF_CFA_PATTERN) {
            let bytes = tiff.raw_bytes(entry)?;
            let two_by_two = [[2, 0, 2, 0], [0, 2, 0, 2]].iter().any(|d| bytes.starts_with(d));
            if two_by_two && bytes.len() >= 8 {
                colors = bytes[4..8].to_vec();
            }
        }
    }
    CfaPattern::new(2, 2, colors)
}

/// The colour table names makers as dcraw does.
fn make_name(make: &str) -> String {
    let upper = make.to_uppercase();
    if upper.starts_with("OLYMPUS") {
        "Olympus".to_owned()
    } else if upper.starts_with("OM DIGITAL") {
        "OM Digital Solutions".to_owned()
    } else {
        make.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpacks_word_swapped_twelve_bit_samples() {
        // Samples 0xABC, 0xDEF, 0x123, 0x456, 0x789, 0xABC, 0xDEF, 0x012 as
        // big-endian bytes, stored as little-endian 32-bit words.
        let big = [0xAB, 0xCD, 0xEF, 0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x12];
        let stored: Vec<u8> = big.chunks(4).flat_map(|w| [w[3], w[2], w[1], w[0]]).collect();
        let samples = unpack_twelve_bits(&stored, 8);
        assert_eq!(samples, [0xABC, 0xDEF, 0x123, 0x456, 0x789, 0xABC, 0xDEF, 0x012]);
    }

    #[test]
    fn names_makers_like_the_colour_table() {
        assert_eq!(make_name("OLYMPUS CORPORATION"), "Olympus");
        assert_eq!(make_name("OLYMPUS IMAGING CORP."), "Olympus");
    }

    /// Decodes a sample named by MIRANTE_OLYMPUS_SAMPLE.
    #[test]
    #[ignore]
    fn decodes_sample_from_environment() {
        let path = std::env::var("MIRANTE_OLYMPUS_SAMPLE").expect("MIRANTE_OLYMPUS_SAMPLE");
        let bytes = std::fs::read(path).unwrap();
        let start = std::time::Instant::now();
        let raw = decode(&bytes).unwrap();
        eprintln!("{:?} {:?} in {:?}", raw.visible, raw.white_balance, start.elapsed());
        eprintln!("{} {} {:?} {} {}", raw.make, raw.model, raw.black, raw.white, raw.orientation);
        if let Ok(out) = std::env::var("MIRANTE_RAW_DUMP") {
            let bytes: Vec<u8> = raw.plane.samples.iter().flat_map(|v| v.to_le_bytes()).collect();
            std::fs::write(out, bytes).unwrap();
        }
    }
}
