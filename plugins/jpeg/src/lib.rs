//! Module: mirante_format_jpeg
//! Purpose: Registers the JPEG plugin (read and write).
//! Created: 2026-11-16
//! Architecture: Decoding lives in decode.rs (DCT) and lossless.rs, encoding in
//! encode.rs; they share markers.rs, huffman.rs, scan.rs and dct.rs.
//! Other plugins may call the public functions re-exported here.

mod blocks;
mod dct;
mod decode;
mod encode;
mod huffman;
mod lossless;
mod markers;
mod metadata;
mod output;
mod scan;

pub use decode::decode;
pub use encode::encode;
pub use lossless::{LosslessFrame, decode_lossless};

use mirante_core::{DecodeError, Decoder, FormatInfo, Image, Magic, PluginRegistry};
use mirante_core::{EncodeError, Encoder};

pub static FORMAT: FormatInfo = FormatInfo {
    id: "jpeg",
    name: "JPEG",
    extensions: &["jpg", "jpeg", "jpe", "jfif"],
    mime_types: &["image/jpeg"],
    magic: &[Magic::at(0, &[0xFF, 0xD8, 0xFF])],
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
