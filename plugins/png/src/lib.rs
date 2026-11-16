//! Module: mirante_format_png
//! Purpose: Registers the PNG plugin (read and write).
//! Created: 2026-11-16
//! Architecture: Decoding lives in decode.rs, encoding in encode.rs.
//! Other plugins may call the public functions re-exported here.

mod decode;
mod encode;
mod filter;

pub use decode::decode;
pub use encode::encode;

use mirante_core::{DecodeError, Decoder, FormatInfo, Image, Magic, PluginRegistry};
use mirante_core::{EncodeError, Encoder};

const SIGNATURE: [u8; 8] = *b"\x89PNG\r\n\x1a\n";

/// Chunk types this plugin reads or writes.
mod chunk {
    pub const IHDR: &[u8; 4] = b"IHDR";
    pub const PLTE: &[u8; 4] = b"PLTE";
    pub const IDAT: &[u8; 4] = b"IDAT";
    pub const IEND: &[u8; 4] = b"IEND";
    pub const TRNS: &[u8; 4] = b"tRNS";
    pub const ICCP: &[u8; 4] = b"iCCP";
    pub const SRGB: &[u8; 4] = b"sRGB";
}

pub static FORMAT: FormatInfo = FormatInfo {
    id: "png",
    name: "PNG",
    extensions: &["png"],
    mime_types: &["image/png"],
    magic: &[Magic::at(0, b"\x89PNG\r\n\x1a\n")],
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
