//! Module: mirante_format_heic
//! Purpose: Registers the HEIC/HEIF plugin (read only).
//! Created: 2026-11-16
//! Architecture: container.rs parses the HEIF boxes, decode.rs builds the primary
//! image, hevc/ is the codec boundary. Other plugins may call `decode`.

mod colour;
mod container;
mod decode;
mod grid;
mod hevc;
mod planes;
mod properties;
#[cfg(test)]
mod testing;
mod transform;
mod ycbcr;

pub use decode::decode;

use mirante_core::{DecodeError, Decoder, FormatInfo, Image, Magic, PluginRegistry};

pub static FORMAT: FormatInfo = FormatInfo {
    id: "heic",
    name: "HEIC/HEIF",
    extensions: &["heic", "heif", "hif"],
    mime_types: &["image/heic", "image/heif", "image/heic-sequence", "image/heif-sequence"],
    magic: &[
        Magic::at(4, b"ftypheic"),
        Magic::at(4, b"ftypheix"),
        Magic::at(4, b"ftyphevc"),
        Magic::at(4, b"ftyphevx"),
        Magic::at(4, b"ftypheim"),
        Magic::at(4, b"ftypheis"),
        Magic::at(4, b"ftypmif1"),
        Magic::at(4, b"ftypmsf1"),
    ],
};

pub fn register(registry: &mut PluginRegistry) {
    registry.add_decoder(Plugin);
}

struct Plugin;

impl Decoder for Plugin {
    fn info(&self) -> &'static FormatInfo {
        &FORMAT
    }

    fn sniff(&self, header: &[u8]) -> bool {
        mirante_codec::isobmff::is_heif(header)
    }

    fn decode(&self, bytes: &[u8]) -> Result<Image, DecodeError> {
        decode(bytes)
    }
}
