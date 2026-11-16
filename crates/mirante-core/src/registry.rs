//! Module: mirante_core::registry
//! Purpose: Holds the registered plugins and picks one for a given file.
//! Created: 2026-11-16
//! Architecture: Content sniffing wins over the file extension, so a mislabelled
//! file still opens. The app builds one registry at startup and shares it.

use crate::{DecodeError, Decoder, Encoder, FormatInfo, Image};
use std::path::Path;

/// How many leading bytes decoders may inspect when sniffing.
pub const SNIFF_LENGTH: usize = 64;

#[derive(Default)]
pub struct PluginRegistry {
    decoders: Vec<Box<dyn Decoder>>,
    encoders: Vec<Box<dyn Encoder>>,
}

impl PluginRegistry {
    pub fn add_decoder(&mut self, decoder: impl Decoder + 'static) {
        self.decoders.push(Box::new(decoder));
    }

    pub fn add_encoder(&mut self, encoder: impl Encoder + 'static) {
        self.encoders.push(Box::new(encoder));
    }

    /// Prefers a decoder matching both content and extension, because RAW files
    /// share TIFF's magic bytes; then content alone; then extension alone.
    pub fn decoder_for(&self, path: &Path, bytes: &[u8]) -> Option<&dyn Decoder> {
        let header = &bytes[..bytes.len().min(SNIFF_LENGTH)];
        let extension = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
        let decoders = || self.decoders.iter().map(Box::as_ref);
        let named = |d: &&dyn Decoder| d.info().has_extension(extension);
        let sniffed = |d: &&dyn Decoder| d.sniff(header);

        decoders()
            .find(|d| sniffed(d) && named(d))
            .or_else(|| decoders().find(sniffed))
            .or_else(|| decoders().find(named))
    }

    pub fn decode(&self, path: &Path, bytes: &[u8]) -> Result<Image, DecodeError> {
        let decoder = self
            .decoder_for(path, bytes)
            .ok_or_else(|| DecodeError::Unsupported(path.display().to_string()))?;
        decoder.decode(bytes)
    }

    pub fn decoder(&self, format_id: &str) -> Option<&dyn Decoder> {
        self.decoders.iter().find(|d| d.info().id == format_id).map(Box::as_ref)
    }

    pub fn encoder(&self, format_id: &str) -> Option<&dyn Encoder> {
        self.encoders.iter().find(|e| e.info().id == format_id).map(Box::as_ref)
    }

    pub fn readable_formats(&self) -> impl Iterator<Item = &'static FormatInfo> + '_ {
        self.decoders.iter().map(|d| d.info())
    }

    pub fn writable_formats(&self) -> impl Iterator<Item = &'static FormatInfo> + '_ {
        self.encoders.iter().map(|e| e.info())
    }

    /// True when some decoder claims the path's extension; used to browse folders.
    pub fn can_open(&self, path: &Path) -> bool {
        let Some(extension) = path.extension().and_then(|e| e.to_str()) else {
            return false;
        };
        self.readable_formats().any(|info| info.has_extension(extension))
    }

    pub fn mime_types(&self) -> Vec<&'static str> {
        let mut types: Vec<_> =
            self.readable_formats().flat_map(|i| i.mime_types).copied().collect();
        types.sort_unstable();
        types.dedup();
        types
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ColorSpace, Magic};

    static FAKE: FormatInfo = FormatInfo {
        id: "fake",
        name: "Fake",
        extensions: &["fk"],
        mime_types: &["image/x-fake"],
        magic: &[Magic::at(0, b"FAKE")],
    };

    struct FakeDecoder;

    impl Decoder for FakeDecoder {
        fn info(&self) -> &'static FormatInfo {
            &FAKE
        }

        fn decode(&self, _: &[u8]) -> Result<Image, DecodeError> {
            Ok(Image::new(1, 1, vec![[0.0; 4]], ColorSpace::Srgb)?)
        }
    }

    fn registry() -> PluginRegistry {
        let mut registry = PluginRegistry::default();
        registry.add_decoder(FakeDecoder);
        registry
    }

    #[test]
    fn content_beats_extension() {
        assert!(registry().decoder_for(Path::new("photo.png"), b"FAKE....").is_some());
    }

    #[test]
    fn extension_is_the_fallback() {
        assert!(registry().decoder_for(Path::new("photo.FK"), b"????").is_some());
        assert!(registry().decoder_for(Path::new("photo.png"), b"????").is_none());
    }
}
