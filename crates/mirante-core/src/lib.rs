//! Module: mirante_core
//! Purpose: The image type every stage shares and the format plugin contracts.
//! Created: 2026-11-16
//! Architecture: Has no GTK, colour-engine or codec dependency, so plugins and
//! tests can use it alone. See ARCHITECTURE.md, "Format plugins".

mod image;
pub mod parallel;
mod plugin;
mod registry;

pub use image::{ColorSpace, Image, ImageError, Rgba};
pub use plugin::{DecodeError, Decoder, EncodeError, Encoder, FormatInfo, MAX_PIXELS, Magic};
pub use registry::{PluginRegistry, SNIFF_LENGTH};
