//! Module: mirante_format_bmp
//! Purpose: Registers the BMP plugin (read only).
//! Created: 2026-11-16
//! Architecture: Decoding lives in decode.rs, RLE expansion in rle.rs.
//! Other plugins may call the public functions re-exported here.

mod decode;
mod rle;

pub use decode::decode;

use mirante_core::{DecodeError, Decoder, FormatInfo, Image, Magic, PluginRegistry};

pub static FORMAT: FormatInfo = FormatInfo {
    id: "bmp",
    name: "BMP",
    extensions: &["bmp", "dib"],
    mime_types: &["image/bmp", "image/x-bmp"],
    magic: &[Magic::at(0, b"BM")],
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
