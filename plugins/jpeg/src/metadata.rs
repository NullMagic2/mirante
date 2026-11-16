//! Module: mirante_format_jpeg::metadata
//! Purpose: Reads the application segments that affect pixels: JFIF, Adobe
//! colour transform, ICC profile chunks and the EXIF orientation.
//! Created: 2026-11-16
//! Architecture: EXIF is parsed with mirante-codec's TIFF reader. Damaged
//! metadata is ignored rather than failing the image.

use crate::markers::{APP0, APP1, APP2, APP14};
use mirante_codec::tiff::{Tiff, tag};

const JFIF_SIGNATURE: &[u8] = b"JFIF\0";
const EXIF_SIGNATURE: &[u8] = b"Exif\0\0";
const ICC_SIGNATURE: &[u8] = b"ICC_PROFILE\0";
const ADOBE_SIGNATURE: &[u8] = b"Adobe";
/// Offset of the transform flag in the Adobe segment: "Adobe", version, two flags.
const ADOBE_TRANSFORM_OFFSET: usize = 11;
const DEFAULT_ORIENTATION: u16 = 1;

#[derive(Default)]
pub struct Metadata<'a> {
    pub jfif: bool,
    pub adobe_transform: Option<u8>,
    orientation: Option<u16>,
    /// (sequence number, chunk count, data) for each ICC_PROFILE segment.
    icc_chunks: Vec<(u8, u8, &'a [u8])>,
}

impl<'a> Metadata<'a> {
    pub fn read(&mut self, marker: u8, payload: &'a [u8]) {
        match marker {
            APP0 if payload.starts_with(JFIF_SIGNATURE) => self.jfif = true,
            APP1 if self.orientation.is_none() => {
                if let Some(tiff) = payload.strip_prefix(EXIF_SIGNATURE) {
                    self.orientation = Some(exif_orientation(tiff));
                }
            }
            APP2 => {
                if let Some([sequence, count, data @ ..]) = payload.strip_prefix(ICC_SIGNATURE) {
                    self.icc_chunks.push((*sequence, *count, data));
                }
            }
            APP14 if payload.starts_with(ADOBE_SIGNATURE) => {
                self.adobe_transform = payload.get(ADOBE_TRANSFORM_OFFSET).copied();
            }
            _ => {}
        }
    }

    pub fn orientation(&self) -> u16 {
        self.orientation.unwrap_or(DEFAULT_ORIENTATION)
    }

    /// The ICC profile, if its chunks are numbered 1..=count without gaps.
    pub fn icc_profile(&self) -> Option<Vec<u8>> {
        let count = self.icc_chunks.first()?.1;
        if usize::from(count) != self.icc_chunks.len() {
            return None;
        }
        let mut profile = Vec::new();
        for sequence in 1..=count {
            let mut chunks = self.icc_chunks.iter().filter(|chunk| chunk.0 == sequence);
            match (chunks.next(), chunks.next()) {
                (Some(chunk), None) if chunk.1 == count => profile.extend_from_slice(chunk.2),
                _ => return None,
            }
        }
        Some(profile)
    }
}

fn exif_orientation(tiff: &[u8]) -> u16 {
    let value = Tiff::parse(tiff)
        .ok()
        .and_then(|tiff| tiff.get_u32(tiff.ifds.first()?, tag::ORIENTATION).ok().flatten());
    match value {
        Some(orientation @ 1..=8) => orientation as u16,
        _ => DEFAULT_ORIENTATION,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn icc_segment(sequence: u8, count: u8, data: &[u8]) -> Vec<u8> {
        [ICC_SIGNATURE, &[sequence, count], data].concat()
    }

    #[test]
    fn joins_icc_chunks_in_sequence_order() {
        let (second, first) = (icc_segment(2, 2, b"cd"), icc_segment(1, 2, b"ab"));
        let mut metadata = Metadata::default();
        metadata.read(APP2, &second);
        metadata.read(APP2, &first);
        assert_eq!(metadata.icc_profile().unwrap(), b"abcd");
    }

    #[test]
    fn rejects_incomplete_icc_profiles() {
        let chunk = icc_segment(1, 2, b"ab");
        let mut metadata = Metadata::default();
        metadata.read(APP2, &chunk);
        assert!(metadata.icc_profile().is_none());
    }
}
