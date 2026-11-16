//! Module: mirante_raw_nikon
//! Purpose: Decodes Nikon RAW files (NEF, NRW) into undeveloped photosites.
//! Created: 2026-11-16
//! Architecture: Returns a NativeRaw (see mirante_raw::frame); the RAW plugin
//! develops it and falls back to the embedded preview when this fails. Bit
//! reading and TIFF steps come from mirante_raw::{bits, tiff_raw}. Levels,
//! crops and names follow dcraw by Dave Coffin and LibRaw 0.21.

pub mod compressed;
pub mod makernote;

use compressed::LinearizationTable;
use makernote::MakerNote;
use mirante_codec::tiff::Tiff;
use mirante_core::DecodeError;
use mirante_raw::tiff_raw::{self, RawIfd, UNCOMPRESSED};
use mirante_raw::{Area, BlackLevel, NativeRaw, unpack::RawPlane};

const MAKER: &str = "Nikon";
const NIKON_COMPRESSED: u32 = 34713;
/// BlackLevel (0x003D) is given at this bit depth.
const BLACK_BITS: u32 = 14;

pub fn decode(bytes: &[u8]) -> Result<NativeRaw, DecodeError> {
    let tiff = Tiff::parse(bytes)?;
    let raw = tiff_raw::find_raw_ifd(&tiff, &[UNCOMPRESSED, NIKON_COMPRESSED])?;
    let exif = tiff_raw::exif_ifd(&tiff)?;
    let note = match tiff_raw::maker_note(&tiff, exif.as_ref())? {
        Some(bytes) => MakerNote::parse(bytes)?,
        None => MakerNote::parse(&[])?,
    };
    let data = tiff_raw::strip_data(&tiff, &raw.ifd)?;
    let full_scale = ((1_u32 << raw.bits.min(16)) - 1) as f32;
    let (samples, white) = match raw.compression {
        UNCOMPRESSED => (tiff_raw::unpack_uncompressed(data, &raw, tiff.endian())?, full_scale),
        _ => decode_compressed(data, &raw, &note)?,
    };
    let pattern = tiff_raw::cfa_pattern(&tiff, &raw.ifd, exif.as_ref())?;
    let model = tiff_raw::model_name(&tiff, MAKER)?;
    let shift = BLACK_BITS.saturating_sub(raw.bits);
    let black = note.black.unwrap_or_default().map(|level| f32::from(level >> shift));
    Ok(NativeRaw {
        visible: visible_area(&model, raw.width, raw.height),
        black: BlackLevel::bayer(&pattern, black),
        white,
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

/// The photosites and the white level the linearisation curve allows.
fn decode_compressed(
    data: &[u8],
    raw: &RawIfd,
    note: &MakerNote<'_>,
) -> Result<(Vec<u16>, f32), DecodeError> {
    // dcraw recognises packed and YUV data stored under this tag by its size.
    let (pixels, bytes) = (raw.width as u64 * raw.height as u64, data.len() as u64);
    let packed_with_padding = (raw.width as u64).div_ceil(10) * 16 * raw.height as u64;
    if [packed_with_padding, pixels * 3 / 2, pixels * 3, pixels * 2].contains(&bytes) {
        return Err(DecodeError::unsupported("Nikon packed or YUV data under compression 34713"));
    }
    let table = note
        .linearization
        .ok_or_else(|| DecodeError::corrupt("compressed NEF without a linearisation table"))?;
    let table = LinearizationTable::parse(table, note.endian, raw.bits)?;
    let samples = compressed::decode(data, raw.width, raw.height, &table)?;
    Ok((samples, f32::from(table.white(raw.bits))))
}

/// Visible area for cameras whose sensor edge dcraw trims; others show it all.
fn visible_area(model: &str, width: usize, height: usize) -> Area {
    let (trim_width, trim_height, left) = match model {
        "D1X" => (4, 0, 0),
        "D40X" | "D60" | "D80" | "D3000" => (4, 3, 0),
        "D3" | "D3S" | "D700" => (4, 0, 2),
        "D3100" => (28, 0, 6),
        "D5000" | "D90" => (42, 0, 0),
        "D5100" | "D7000" | "COOLPIX A" => (44, 0, 0),
        "D3200" | "D600" | "D610" | "D800" | "D800E" => (46, 0, 0),
        "D4" | "Df" => (52, 0, 2),
        "D40" | "D50" | "D70" | "D70s" => (1, 0, 0),
        "D200" => (4, 0, 1),
        "D2H" | "D2Hs" => (14, 0, 6),
        "D2X" | "D2Xs" if width == 3264 => (32, 0, 0),
        "D2X" | "D2Xs" => (8, 0, 0),
        "D300" | "D300S" => (32, 0, 0),
        _ if model.starts_with("1 ") => (0, 2, 0),
        _ => (0, 0, 0),
    };
    if left + trim_width >= width || trim_height >= height {
        return Area { left: 0, top: 0, width, height };
    }
    Area { left, top: 0, width: width - trim_width, height: height - trim_height }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_known_sensor_edges() {
        let area = visible_area("D3", 4288, 2848);
        assert_eq!(area, Area { left: 2, top: 0, width: 4284, height: 2848 });
        assert_eq!(visible_area("D750", 6032, 4032).width, 6032);
    }

    /// Set MIRANTE_NEF to a NEF path to decode a real file.
    #[test]
    #[ignore]
    fn decodes_sample_from_environment() {
        let path = std::env::var("MIRANTE_NEF").expect("MIRANTE_NEF");
        let bytes = std::fs::read(path).unwrap();
        let start = std::time::Instant::now();
        let raw = decode(&bytes).unwrap();
        eprintln!("{} {} in {:?}: {:?}", raw.make, raw.model, start.elapsed(), raw.visible);
        eprintln!("black {:?} white {} wb {:?}", raw.black, raw.white, raw.white_balance);
    }
}
