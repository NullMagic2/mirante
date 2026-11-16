//! Module: mirante_raw_pentax
//! Purpose: Decodes Pentax RAW files (PEF) into undeveloped photosites.
//! Created: 2026-11-16
//! Architecture: Returns a NativeRaw (see mirante_raw::frame); the RAW plugin
//! develops it and falls back to the embedded preview when this fails. The TIFF
//! steps and bit-level primitives come from mirante_raw::{bits, tiff_raw}. Crops
//! and levels follow dcraw by Dave Coffin and LibRaw 0.21.

pub mod compressed;
pub mod makernote;

use makernote::MakerNote;
use mirante_codec::tiff::Tiff;
use mirante_core::DecodeError;
use mirante_raw::demosaic::{BLUE, CfaPattern, GREEN, RED};
use mirante_raw::tiff_raw::{self, RawIfd, UNCOMPRESSED};
use mirante_raw::{Area, BlackLevel, NativeRaw, unpack::RawPlane};

const MAKER: &str = "Pentax";
const PENTAX_COMPRESSED: u32 = 65535;

pub fn decode(bytes: &[u8]) -> Result<NativeRaw, DecodeError> {
    let tiff = Tiff::parse(bytes)?;
    let raw = tiff_raw::find_raw_ifd(&tiff, &[UNCOMPRESSED, PENTAX_COMPRESSED])?;
    let exif = tiff_raw::exif_ifd(&tiff)?;
    let note = match tiff_raw::maker_note(&tiff, exif.as_ref())? {
        Some(note) => MakerNote::parse(bytes, note)?,
        None => MakerNote::default(),
    };
    let data = tiff_raw::strip_data(&tiff, &raw.ifd)?;
    let samples = match raw.compression {
        UNCOMPRESSED => tiff_raw::unpack_uncompressed(data, &raw, tiff.endian())?,
        _ => {
            let (Some(table), Some(endian)) = (note.huffman, note.endian) else {
                return Err(DecodeError::corrupt("compressed PEF without a Huffman table"));
            };
            let table = compressed::huffman_table(table, endian)?;
            compressed::decode(data, raw.width, raw.height, raw.bits, &table)?
        }
    };
    let model = tiff_raw::model_name(&tiff, MAKER)?;
    let crop = crop(&model, &raw);
    let pattern = if crop.bggr {
        CfaPattern::new(2, 2, vec![BLUE, GREEN, GREEN, RED])?
    } else {
        tiff_raw::cfa_pattern(&tiff, &raw.ifd, exif.as_ref())?
    };
    Ok(NativeRaw {
        visible: crop.visible,
        black: BlackLevel::bayer(&pattern, note.black.unwrap_or_default()),
        white: ((1_u32 << raw.bits.min(16)) - 1) as f32,
        white_balance: note.white_balance,
        pattern: Some(pattern),
        plane: RawPlane {
            width: raw.width,
            height: raw.height,
            samples_per_pixel: 1,
            bits_per_sample: raw.bits,
            samples,
        },
        make: MAKER.to_owned(),
        model,
        orientation: tiff_raw::orientation(&tiff)?,
    })
}

struct Crop {
    visible: Area,
    /// dcraw overrides the file's CFA pattern for these models; relative to the
    /// raw origin it is always BGGR.
    bggr: bool,
}

/// Masked borders per model, from dcraw's identify and LibRaw's
/// identify_finetune_dcr (K-1, KP). Unlisted models show the whole sensor.
fn crop(model: &str, raw: &RawIfd) -> Crop {
    let (width, height) = (raw.width, raw.height);
    let area = |left, top, width, height| Area { left, top, width, height };
    let (visible, bggr) = match (model, width, height) {
        (_, 3936, 2624) => (area(0, 0, 3896, 2616), false),
        (_, 4864, 3136) if model != "K-7" => (area(0, 0, 4688, 3124), true),
        ("K-r" | "K-x", _, 2868) => (area(0, 0, 4309, height), true),
        ("K-7", _, 3136) => (area(0, 2, 4684, 3122), true),
        (_, _, 3284) if model.starts_with("K-5") => (area(10, 0, 4950, height), true),
        (_, _, 3300) if model.starts_with("K-50") => (area(0, 12, 4952, 3288), false),
        (_, _, 3664) if model.starts_with("K-S") => (area(0, 0, 5492, height), false),
        ("K-3", _, 4032) => (area(4, 0, 6040, height), false),
        ("KP", 6112, _) => (area(54, 28, 6028, height.saturating_sub(28)), false),
        ("K-1" | "K-1 Mark II", 7392, _) => (area(6, 18, 7376, height.saturating_sub(18)), false),
        ("K-1" | "K-1 Mark II", _, _) => (area(0, 18, width, height.saturating_sub(18)), false),
        ("645D", _, 5552) => (area(48, 29, 7328, 5502), true),
        _ => (area(0, 0, width, height), false),
    };
    let fits = visible.width > 0
        && visible.height > 0
        && visible.left + visible.width <= width
        && visible.top + visible.height <= height;
    if fits {
        Crop { visible, bggr }
    } else {
        Crop { visible: area(0, 0, width, height), bggr: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante_codec::tiff::Ifd;

    fn raw(width: usize, height: usize) -> RawIfd {
        RawIfd { ifd: Ifd::default(), width, height, bits: 14, compression: PENTAX_COMPRESSED }
    }

    #[test]
    fn crops_known_models() {
        let k1 = crop("K-1", &raw(7392, 4950)).visible;
        assert_eq!(k1, Area { left: 6, top: 18, width: 7376, height: 4932 });
        assert!(crop("K-5 II s", &raw(4992, 3284)).bggr);
        let unknown = crop("K-1", &raw(100, 10)).visible;
        assert_eq!(unknown, Area { left: 0, top: 0, width: 100, height: 10 });
    }

    /// Set MIRANTE_PEF to a PEF path to decode a real file.
    #[test]
    #[ignore]
    fn decodes_sample_from_environment() {
        let path = std::env::var("MIRANTE_PEF").expect("MIRANTE_PEF");
        let bytes = std::fs::read(path).unwrap();
        let start = std::time::Instant::now();
        let raw = decode(&bytes).unwrap();
        eprintln!("{} {} in {:?}: {:?}", raw.make, raw.model, start.elapsed(), raw.visible);
        eprintln!("black {:?} white {} wb {:?}", raw.black, raw.white, raw.white_balance);
    }
}
