//! Module: mirante_codec
//! Purpose: Building blocks shared by more than one format plugin.
//! Created: 2026-11-16
//! Architecture: Each module is a self-contained primitive with no knowledge of
//! a particular file format. Plugins compose them; nothing here registers itself.

pub mod checksum;
pub mod color_models;
pub mod deflate;
pub mod inflate;
pub mod isobmff;
pub mod packbits;
pub mod reader;
pub mod tar;
pub mod tiff;

pub use reader::{ByteReader, Endian};
