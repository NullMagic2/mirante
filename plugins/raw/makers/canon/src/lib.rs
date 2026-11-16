//! Module: mirante_raw_canon
//! Purpose: Decodes Canon RAW files (CR2, CR3) into undeveloped photosites.
//! Created: 2026-11-16
//! Architecture: Returns a NativeRaw (see mirante_raw::frame); the RAW plugin
//! develops it and falls back to the embedded preview when this fails. cr2.rs
//! and cr3.rs read the containers, crx/ the CR3 codec, maker_note.rs the Canon
//! metadata, assemble.rs builds the result.

mod assemble;
mod cr2;
mod cr3;
mod crx;
mod ljpeg;
mod maker_note;
mod sraw;

use mirante_codec::isobmff::FileType;
use mirante_codec::tiff::Tiff;
use mirante_core::DecodeError;
use mirante_raw::NativeRaw;

const CR3_BRAND: [u8; 4] = *b"crx ";

pub fn decode(bytes: &[u8]) -> Result<NativeRaw, DecodeError> {
    if Tiff::is_tiff(bytes) {
        return cr2::decode(bytes);
    }
    match FileType::read(bytes) {
        Some(file_type) if file_type.major_brand == CR3_BRAND || file_type.has_brand(CR3_BRAND) => {
            cr3::decode(bytes)
        }
        _ => Err(DecodeError::unsupported("not a Canon CR2 or CR3 file")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_other_files() {
        assert!(decode(b"not a raw file at all").is_err());
        assert!(decode(b"II*\0\x08\0\0\0\0\0").is_err());
    }

    /// Decodes the file named by MIRANTE_CANON_SAMPLE (a real CR2 or CR3).
    #[test]
    #[ignore]
    fn decodes_a_sample_file() {
        let path = std::env::var("MIRANTE_CANON_SAMPLE").expect("MIRANTE_CANON_SAMPLE is set");
        let raw = decode(&std::fs::read(path).unwrap()).unwrap();
        let plane = &raw.plane;
        assert_eq!(plane.samples.len(), plane.width * plane.height * plane.samples_per_pixel);
        assert!(raw.visible.left + raw.visible.width <= plane.width);
        assert!(raw.visible.top + raw.visible.height <= plane.height);
        assert_eq!(raw.make, "Canon");
        assert!(raw.model.starts_with("EOS") || raw.model.starts_with("PowerShot"));
    }
}
