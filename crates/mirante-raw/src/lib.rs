//! Module: mirante_raw
//! Purpose: The parts of RAW decoding that do not depend on the camera maker.
//! Created: 2026-11-16
//! Architecture: A maker crate decodes its file format into a `NativeRaw`
//! (photosites, CFA pattern, levels, white balance, camera name); `develop`
//! turns any NativeRaw into a linear Rec. 2020 Image. DNG uses the same parts.

pub mod bits;
pub mod camera;
pub mod camera_table;
pub mod color;
pub mod demosaic;
pub mod develop;
mod fit;
pub mod frame;
pub mod levels;
pub mod tiff_raw;
pub mod unpack;

pub use develop::develop;
pub use frame::{Area, BlackLevel, NativeRaw};
