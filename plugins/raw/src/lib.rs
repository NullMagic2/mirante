//! Module: mirante_format_raw
//! Purpose: Registers the camera RAW plugin (read only).
//! Created: 2026-11-16
//! Architecture: decode.rs dispatches on the container (sniff.rs): DNG raw data is
//! developed by dng.rs, other RAWs show their embedded preview (preview.rs).
//! Other plugins may call the public functions re-exported here.

mod decode;
mod dng;
mod preview;
mod sniff;
#[cfg(test)]
mod testing;

pub use decode::decode;

use mirante_core::{DecodeError, Decoder, FormatInfo, Image, Magic, PluginRegistry};

pub static FORMAT: FormatInfo = FormatInfo {
    id: "raw",
    name: "Camera RAW",
    extensions: &[
        "dng", "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "orf", "rw2", "raf", "pef",
        "srw", "erf", "kdc", "dcr", "mrw", "3fr", "iiq", "rwl",
    ],
    mime_types: &[
        "image/x-adobe-dng",
        "image/x-canon-cr2",
        "image/x-canon-cr3",
        "image/x-canon-crw",
        "image/x-nikon-nef",
        "image/x-nikon-nrw",
        "image/x-sony-arw",
        "image/x-sony-srf",
        "image/x-sony-sr2",
        "image/x-olympus-orf",
        "image/x-panasonic-rw2",
        "image/x-fuji-raf",
        "image/x-pentax-pef",
        "image/x-samsung-srw",
        "image/x-epson-erf",
        "image/x-kodak-kdc",
        "image/x-kodak-dcr",
        "image/x-minolta-mrw",
        "image/x-hasselblad-3fr",
        "image/x-phaseone-iiq",
        "image/x-leica-rwl",
        "image/x-dcraw",
    ],
    magic: &[
        Magic::at(0, b"II*\0"),
        Magic::at(0, b"MM\0*"),
        Magic::at(0, b"IIRO"),
        Magic::at(0, b"IIRS"),
        Magic::at(0, b"MMOR"),
        Magic::at(0, b"IIU\0"),
        Magic::at(0, b"FUJIFILMCCD-RAW"),
        Magic::at(4, b"ftypcrx "),
        Magic::at(0, b"\0MRM"),
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
        sniff::looks_like_raw(header)
    }

    fn decode(&self, bytes: &[u8]) -> Result<Image, DecodeError> {
        decode(bytes)
    }
}
