//! Module: mirante_format_tiff
//! Purpose: Registers the TIFF plugin (read and write).
//! Created: 2026-11-16
//! Architecture: Decoding starts in decode.rs, encoding lives in encode.rs.
//! Other plugins may call the public functions re-exported here.

mod decode;
mod encode;
mod jpeg;
mod layout;
mod lzw;
mod pixels;
mod samples;

pub use decode::decode;
pub use encode::encode;

use mirante_core::{DecodeError, Decoder, FormatInfo, Image, Magic, PluginRegistry};
use mirante_core::{EncodeError, Encoder};

pub static FORMAT: FormatInfo = FormatInfo {
    id: "tiff",
    name: "TIFF",
    extensions: &["tif", "tiff"],
    mime_types: &["image/tiff"],
    magic: &[Magic::at(0, b"II*\0"), Magic::at(0, b"MM\0*")],
};

pub fn register(registry: &mut PluginRegistry) {
    registry.add_decoder(Plugin);
    registry.add_encoder(Plugin);
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

impl Encoder for Plugin {
    fn info(&self) -> &'static FormatInfo {
        &FORMAT
    }

    fn encode(&self, image: &Image) -> Result<Vec<u8>, EncodeError> {
        encode(image)
    }
}
