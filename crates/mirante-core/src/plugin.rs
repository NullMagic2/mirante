//! Module: mirante_core::plugin
//! Purpose: The contracts a format plugin implements to read or write files.
//! Created: 2026-11-16
//! Architecture: A plugin crate exposes `register(&mut PluginRegistry)` and adds
//! one Decoder, and optionally one Encoder, per format it owns.

use crate::{Image, ImageError};
use std::fmt;

/// Bytes found at a fixed offset at the start of every file of a format.
#[derive(Debug)]
pub struct Magic {
    pub offset: usize,
    pub bytes: &'static [u8],
}

impl Magic {
    pub const fn at(offset: usize, bytes: &'static [u8]) -> Self {
        Self { offset, bytes }
    }

    pub fn matches(&self, header: &[u8]) -> bool {
        header.get(self.offset..self.offset + self.bytes.len()) == Some(self.bytes)
    }
}

/// Static description of a format; the registry, plugin manifests and the
/// .desktop file read it. Magic is declarative so no plugin runs just to sniff.
#[derive(Debug)]
pub struct FormatInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub extensions: &'static [&'static str],
    pub mime_types: &'static [&'static str],
    pub magic: &'static [Magic],
}

impl FormatInfo {
    pub fn has_extension(&self, extension: &str) -> bool {
        self.extensions.iter().any(|known| known.eq_ignore_ascii_case(extension))
    }
}

#[derive(Debug)]
pub enum DecodeError {
    Unsupported(String),
    Corrupt(String),
    Image(ImageError),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(reason) => write!(formatter, "unsupported file: {reason}"),
            Self::Corrupt(reason) => write!(formatter, "damaged file: {reason}"),
            Self::Image(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Largest image any decoder may allocate; 2^29 pixels is 8 GiB of working floats.
pub const MAX_PIXELS: u64 = 1 << 29;

impl DecodeError {
    pub fn corrupt(reason: impl Into<String>) -> Self {
        Self::Corrupt(reason.into())
    }

    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self::Unsupported(reason.into())
    }

    /// Validates header dimensions before any allocation and returns the pixel count.
    pub fn check_size(width: u32, height: u32) -> Result<usize, Self> {
        let pixels = u64::from(width) * u64::from(height);

        if pixels == 0 || pixels > MAX_PIXELS {
            return Err(Self::unsupported(format!("image size {width}x{height}")));
        }
        Ok(pixels as usize)
    }
}

impl From<ImageError> for DecodeError {
    fn from(error: ImageError) -> Self {
        Self::Image(error)
    }
}

#[derive(Debug)]
pub struct EncodeError(pub String);

impl fmt::Display for EncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for EncodeError {}

pub trait Decoder: Send + Sync {
    fn info(&self) -> &'static FormatInfo;

    /// True when the first bytes of a file look like this format.
    fn sniff(&self, header: &[u8]) -> bool {
        self.info().magic.iter().any(|magic| magic.matches(header))
    }

    /// Returns pixels in the file's own colour space, tagged accordingly.
    fn decode(&self, bytes: &[u8]) -> Result<Image, DecodeError>;
}

pub trait Encoder: Send + Sync {
    fn info(&self) -> &'static FormatInfo;

    /// `image` is sRGB-encoded with values in 0..1; files are written untagged or
    /// marked sRGB, which every reader treats as sRGB.
    fn encode(&self, image: &Image) -> Result<Vec<u8>, EncodeError>;
}
