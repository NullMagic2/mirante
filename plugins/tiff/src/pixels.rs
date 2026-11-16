//! Module: mirante_format_tiff::pixels
//! Purpose: Turns gathered TIFF samples into straight-alpha RGBA pixels.
//! Created: 2026-11-16
//! Architecture: Applies the photometric interpretation; CMYK goes through the
//! shared naive model in mirante_codec::color_models.

use crate::layout::{Layout, photometric};
use crate::samples::SampleFormat;
use mirante_codec::color_models::cmyk_to_rgb;
use mirante_core::Rgba;

/// `samples` holds the colour channels then, if present, alpha for each pixel.
pub fn convert(samples: &[f32], layout: &Layout, color_map: Option<&[f32]>) -> Vec<Rgba> {
    let channels = layout.color_channels;
    let slots = channels + usize::from(layout.alpha.is_some());
    let scale = match layout.row.format {
        SampleFormat::Float => 1.0,
        SampleFormat::Unsigned => 1.0 / ((1_u32 << layout.row.bits) - 1) as f32,
    };
    let associated = layout.alpha.is_some_and(|alpha| alpha.associated);
    // Rounding in premultiplied integers can push a channel just past full.
    let ceiling = match layout.row.format {
        SampleFormat::Float => f32::INFINITY,
        SampleFormat::Unsigned => 1.0,
    };

    samples
        .chunks_exact(slots)
        .map(|pixel| {
            let alpha = pixel.get(channels).map_or(1.0, |&a| (a * scale).clamp(0.0, 1.0));
            if layout.photometric == photometric::PALETTE {
                let map = color_map.expect("palette images carry a validated colour map");
                let (index, size) = (pixel[0] as usize, 1 << layout.row.bits);
                let channel = |plane: usize| map[plane * size + index];
                return [channel(0), channel(1), channel(2), alpha];
            }
            let mut color = [0.0; 4];
            for (value, &stored) in color.iter_mut().zip(&pixel[..channels]) {
                *value = stored * scale;
                if associated && alpha > 0.0 {
                    *value = (*value / alpha).min(ceiling);
                }
            }
            let [r, g, b] = match layout.photometric {
                photometric::WHITE_IS_ZERO => [1.0 - color[0]; 3],
                photometric::BLACK_IS_ZERO => [color[0]; 3],
                photometric::SEPARATED => cmyk_to_rgb(color),
                _ => [color[0], color[1], color[2]],
            };
            [r, g, b, alpha]
        })
        .collect()
}
