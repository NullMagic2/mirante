//! Module: mirante_edit::text
//! Purpose: Edits as `key=value` lines, the form the viewer stores them in.
//! Created: 2026-11-16
//! Architecture: Unknown keys and unreadable values are skipped, so files from
//! newer or older versions still load what they can.

use crate::{CropRect, Edits};

impl Edits {
    pub fn to_text(&self) -> String {
        let a = &self.adjustments;
        let mut text = String::new();
        let o = self.orientation;
        if o.quarter_turns != 0 {
            text.push_str(&format!("rotate={}\n", u16::from(o.quarter_turns) * 90));
        }
        if o.mirrored {
            text.push_str("mirror=yes\n");
        }
        if let Some(c) = self.crop {
            text.push_str(&format!("crop={},{},{},{}\n", c.x, c.y, c.width, c.height));
        }
        let values = [
            ("exposure", a.exposure),
            ("contrast", a.contrast),
            ("saturation", a.saturation),
            ("temperature", a.temperature),
            ("tint", a.tint),
        ];
        for (key, value) in values.iter().filter(|(_, v)| *v != 0.0) {
            text.push_str(&format!("{key}={value}\n"));
        }
        text
    }

    pub fn from_text(text: &str) -> Self {
        let mut edits = Self::default();
        for (key, value) in text.lines().filter_map(|line| line.split_once('=')) {
            let number = || value.trim().parse::<f32>().ok().filter(|v| v.is_finite());
            let a = &mut edits.adjustments;
            match key.trim() {
                "crop" => edits.crop = crop(value),
                "rotate" => {
                    let degrees = value.trim().parse::<u16>().unwrap_or_default();
                    edits.orientation.quarter_turns = ((degrees / 90) % 4) as u8;
                }
                "mirror" => edits.orientation.mirrored = value.trim() == "yes",
                "exposure" => a.exposure = number().unwrap_or_default(),
                "contrast" => a.contrast = number().unwrap_or_default(),
                "saturation" => a.saturation = number().unwrap_or_default(),
                "temperature" => a.temperature = number().unwrap_or_default(),
                "tint" => a.tint = number().unwrap_or_default(),
                _ => {}
            }
        }
        edits
    }
}

fn crop(value: &str) -> Option<CropRect> {
    let parts: Vec<u32> = value.split(',').map(|v| v.trim().parse().ok()).collect::<Option<_>>()?;
    let [x, y, width, height] = parts[..] else { return None };
    (width > 0 && height > 0).then_some(CropRect { x, y, width, height })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Adjustments;

    #[test]
    fn round_trips() {
        let edits = Edits {
            crop: Some(CropRect { x: 1, y: 2, width: 30, height: 40 }),
            orientation: crate::Orientation { quarter_turns: 3, mirrored: true },
            adjustments: Adjustments { exposure: 0.5, tint: -0.25, ..Default::default() },
        };
        assert_eq!(Edits::from_text(&edits.to_text()), edits);
        assert_eq!(Edits::default().to_text(), "");
    }

    #[test]
    fn skips_what_it_cannot_read() {
        let edits = Edits::from_text("crop=1,2\nexposure=abc\nfuture=1\ncontrast=0.5");
        assert_eq!(edits.crop, None);
        assert_eq!(edits.adjustments, Adjustments { contrast: 0.5, ..Default::default() });
    }
}
