//! Module: mirante_codec::color_models
//! Purpose: Device colour model conversions decoders share (YCbCr, CMYK).
//! Created: 2026-11-16
//! Architecture: Results are sRGB-encoded and tagged sRGB by the caller. CMYK uses
//! the naive formula because CMYK ICC profiles are LUT-based, which we do not run.

/// JFIF / BT.601 full-range YCbCr to RGB. `y` is 0..1; `cb` and `cr` are signed
/// offsets from neutral chroma, because each format centres chroma differently.
pub fn ycbcr_to_rgb(y: f32, cb: f32, cr: f32) -> [f32; 3] {
    [y + 1.402 * cr, y - 0.344_136 * cb - 0.714_136 * cr, y + 1.772 * cb].map(|v| v.clamp(0.0, 1.0))
}

/// CMYK (0 = no ink) to RGB, all channels 0..1.
pub fn cmyk_to_rgb([c, m, y, k]: [f32; 4]) -> [f32; 3] {
    [c, m, y].map(|ink| (1.0 - ink) * (1.0 - k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_chroma_is_grey() {
        assert_eq!(ycbcr_to_rgb(0.25, 0.0, 0.0), [0.25; 3]);
    }

    #[test]
    fn full_black_ink_is_black() {
        assert_eq!(cmyk_to_rgb([0.0, 0.0, 0.0, 1.0]), [0.0; 3]);
    }
}
