//! Module: mirante_format_raw::decode
//! Purpose: Decodes camera RAW files into an Image.
//! Created: 2026-11-16
//! Architecture: DNG is developed by dng.rs; other formats go to their maker
//! crate (plugins/raw/makers) and mirante_raw::develop. When a maker decoder
//! fails, the embedded JPEG preview (preview.rs) is shown instead.

use crate::sniff::{self, Container};
use crate::{
    dng,
    preview::{self, Want},
};
use mirante_codec::tiff::{Tiff, tag};
use mirante_core::{DecodeError, Image};
use mirante_raw::NativeRaw;

type MakerDecoder = fn(&[u8]) -> Result<NativeRaw, DecodeError>;
/// Colour calibration needs only a small preview; decoding the largest is slow.
const CALIBRATION: Want = Want::AtLeast(640 * 420);

pub fn decode(bytes: &[u8]) -> Result<Image, DecodeError> {
    match sniff::container(bytes) {
        Some(Container::Tiff) => decode_tiff(bytes),
        Some(Container::Raf) => {
            developed(mirante_raw_fuji::decode(bytes), &|want| preview::from_raf(bytes, want))
        }
        Some(Container::Cr3) => {
            developed(mirante_raw_canon::decode(bytes), &|want| preview::from_cr3(bytes, want))
        }
        Some(Container::Mrw) => Err(DecodeError::unsupported("Minolta MRW files")),
        Some(Container::Crw) => Err(DecodeError::unsupported("Canon CRW files")),
        None => Err(DecodeError::unsupported("not a known camera RAW container")),
    }
}

fn decode_tiff(bytes: &[u8]) -> Result<Image, DecodeError> {
    let tiff = Tiff::parse(bytes)?;
    if dng::is_dng(&tiff) {
        return match dng::decode(&tiff) {
            // A preview beats an error, but only when the raw data is merely unsupported.
            Err(DecodeError::Unsupported(reason)) => preview::from_tiff(&tiff, Want::Largest)
                .map_err(|_| DecodeError::Unsupported(reason)),
            result => result,
        };
    }
    match maker_decoder(&tiff) {
        Some(decoder) => developed(decoder(bytes), &|want| preview::from_tiff(&tiff, want)),
        None => preview::from_tiff(&tiff, Want::Largest),
    }
}

/// Picks the maker crate from IFD0's Make tag.
fn maker_decoder(tiff: &Tiff<'_>) -> Option<MakerDecoder> {
    let entry = tiff.ifds.first()?.get(tag::MAKE)?;
    let make = tiff.ascii(entry).ok()?.to_lowercase();
    let decoders: [(&[&str], MakerDecoder); 6] = [
        (&["canon"], mirante_raw_canon::decode),
        (&["nikon"], mirante_raw_nikon::decode),
        (&["sony"], mirante_raw_sony::decode),
        (&["olympus", "om digital"], mirante_raw_olympus::decode),
        (&["panasonic", "leica"], mirante_raw_panasonic::decode),
        (&["pentax", "ricoh"], mirante_raw_pentax::decode),
    ];
    decoders
        .into_iter()
        .find(|(names, _)| names.iter().any(|name| make.starts_with(name)))
        .map(|(_, decoder)| decoder)
}

/// Develops native data; on any failure shows the camera's own preview instead.
fn developed(
    native: Result<NativeRaw, DecodeError>,
    preview: &dyn Fn(Want) -> Result<Image, DecodeError>,
) -> Result<Image, DecodeError> {
    let result = native.and_then(|raw| mirante_raw::develop(raw, &|| preview(CALIBRATION).ok()));
    result.or_else(|error| {
        eprintln!("mirante: showing the embedded preview instead: {error}");
        preview(Want::Largest).map_err(|_| error)
    })
}
