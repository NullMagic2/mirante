//! Module: mirante_format_raw::sniff
//! Purpose: Recognises camera RAW containers from their first bytes.
//! Created: 2026-11-16
//! Architecture: TIFF-based RAWs share TIFF's magic, so the registry also checks
//! the extension; see PluginRegistry::decoder_for.

use mirante_codec::isobmff::FileType;

/// The container families decode.rs dispatches on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    /// TIFF and its variants: DNG, CR2, NEF, ARW, PEF, ORF, RW2 and others.
    Tiff,
    /// Fujifilm RAF.
    Raf,
    /// Canon CR3 (ISO-BMFF).
    Cr3,
    /// Minolta MRW.
    Mrw,
    /// Canon CRW (CIFF).
    Crw,
}

/// TIFF byte-order marks with the standard, Olympus and Panasonic magic numbers.
const TIFF_MAGICS: [&[u8]; 6] = [b"II*\0", b"MM\0*", b"IIRO", b"IIRS", b"MMOR", b"IIU\0"];
const RAF_MAGIC: &[u8] = b"FUJIFILMCCD-RAW";
const MRW_MAGIC: &[u8] = b"\0MRM";
const CRW_SIGNATURE: &[u8] = b"HEAPCCDR";
const CRW_SIGNATURE_AT: usize = 6;
const CR3_BRAND: [u8; 4] = *b"crx ";

pub fn container(header: &[u8]) -> Option<Container> {
    if TIFF_MAGICS.iter().any(|magic| header.starts_with(magic)) {
        Some(Container::Tiff)
    } else if header.starts_with(RAF_MAGIC) {
        Some(Container::Raf)
    } else if header.starts_with(MRW_MAGIC) {
        Some(Container::Mrw)
    } else if header.starts_with(b"II")
        && header.get(CRW_SIGNATURE_AT..).is_some_and(|s| s.starts_with(CRW_SIGNATURE))
    {
        Some(Container::Crw)
    } else if FileType::read(header).is_some_and(|file_type| file_type.major_brand == CR3_BRAND) {
        Some(Container::Cr3)
    } else {
        None
    }
}

pub fn looks_like_raw(header: &[u8]) -> bool {
    container(header).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_raw_containers() {
        assert_eq!(container(b"II*\0\x08\0\0\0"), Some(Container::Tiff));
        assert_eq!(container(b"IIRO\x08\0\0\0"), Some(Container::Tiff));
        assert_eq!(container(b"IIU\0\x08\0\0\0"), Some(Container::Tiff));
        assert_eq!(container(b"FUJIFILMCCD-RAW 0201"), Some(Container::Raf));
        assert_eq!(container(b"\0MRM\0\0"), Some(Container::Mrw));
        assert_eq!(container(b"II\x1a\0\0\0HEAPCCDR"), Some(Container::Crw));
        assert_eq!(container(b"\0\0\0\x18ftypcrx \0\0\0\x01crx isom"), Some(Container::Cr3));
        assert!(!looks_like_raw(b"\0\0\0\x18ftypheic\0\0\0\0mif1heic"));
        assert!(!looks_like_raw(b"\x89PNG\r\n"));
    }
}
