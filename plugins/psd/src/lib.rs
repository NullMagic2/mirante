//! Module: mirante_format_psd
//! Purpose: Registers the Photoshop PSD/PSB plugin (read only).
//! Created: 2026-11-16
//! Architecture: Decoding lives in decode.rs, section parsing in sections.rs.
//! Other plugins may call the public functions re-exported here.

mod decode;
mod sections;

pub use decode::decode;

use mirante_core::{DecodeError, Decoder, FormatInfo, Image, Magic, PluginRegistry};

pub static FORMAT: FormatInfo = FormatInfo {
    id: "psd",
    name: "Photoshop PSD",
    extensions: &["psd", "psb"],
    mime_types: &["image/vnd.adobe.photoshop"],
    magic: &[Magic::at(0, b"8BPS\0\x01"), Magic::at(0, b"8BPS\0\x02")],
};

pub fn register(registry: &mut PluginRegistry) {
    registry.add_decoder(Plugin);
}

struct Plugin;

impl Decoder for Plugin {
    fn info(&self) -> &'static FormatInfo {
        &FORMAT
    }

    fn decode(&self, bytes: &[u8]) -> Result<Image, DecodeError> {
        decode(bytes)
    }
}
