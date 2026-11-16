//! Module: mirante_raw_canon::maker_note
//! Purpose: Reads the Canon maker-note tags a decoder needs: SensorInfo
//! (visible area), ColorData (as-shot white balance, black and white levels,
//! sRAW multipliers), the model id and the firmware version.
//! Created: 2026-11-16
//! Architecture: Works on any TIFF IFD, so CR2 (EXIF MakerNote) and CR3 (CMT3,
//! CTMD records) share it. ColorData offsets follow ExifTool's Canon tables;
//! black/white offsets are used only where checked against real files.

use mirante_codec::tiff::{Ifd, Tiff};
use mirante_core::DecodeError;
use mirante_raw::Area;

const FIRMWARE: u16 = 0x0007;
const MODEL_ID: u16 = 0x0010;
const SENSOR_INFO: u16 = 0x00E0;
const COLOR_DATA: u16 = 0x4001;
/// SensorInfo: width, height, then left, top, right and bottom (inclusive) borders.
const SENSOR_FIELDS: [usize; 6] = [1, 2, 5, 6, 7, 8];
/// dcraw stops its sRAW multiplier search at the entry whose green is this.
const SRAW_GREEN_MARK: u16 = 1170;
/// Specular white follows the four black levels and the normal white level.
const SPECULAR_AFTER_BLACK: usize = 5;
const MAX_BLACK_SPREAD: u16 = 128;
const MIN_WHITE_ABOVE_BLACK: u16 = 1024;

/// Where one ColorData version keeps its fields, by its length in shorts.
struct Layout {
    counts: &'static [usize],
    as_shot: usize,
    /// First of the four per-channel black levels (R, G, G, B).
    levels: Option<usize>,
}

const LAYOUTS: [Layout; 11] = [
    Layout { counts: &[582], as_shot: 0x19, levels: None },
    Layout { counts: &[653], as_shot: 0x22, levels: None },
    Layout {
        counts: &[796, 674, 692, 702, 1227, 1250, 1251, 1337, 1338, 1346, 1273, 1275],
        as_shot: 0x3F,
        levels: None,
    },
    Layout { counts: &[1312], as_shot: 0x3F, levels: Some(0x1F8) },
    Layout { counts: &[1313, 1316, 1506], as_shot: 0x3F, levels: None },
    Layout { counts: &[1592], as_shot: 0x3F, levels: Some(0x30A) },
    Layout { counts: &[1560, 1353, 1602], as_shot: 0x3F, levels: None },
    Layout { counts: &[5120, 1816, 1820, 1824], as_shot: 0x47, levels: None },
    Layout { counts: &[3656], as_shot: 0x55, levels: Some(0x326) },
    Layout { counts: &[2024], as_shot: 0x55, levels: None },
    Layout { counts: &[3973, 3778], as_shot: 0x69, levels: None },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SensorInfo {
    pub width: usize,
    pub height: usize,
    pub left: usize,
    pub top: usize,
    pub right: usize,
    pub bottom: usize,
}

impl SensorInfo {
    /// The image part of a plane of the given size, if the borders describe it.
    pub fn visible(&self, width: usize, height: usize) -> Option<Area> {
        let fits = (self.width, self.height) == (width, height)
            && self.left <= self.right
            && self.top <= self.bottom
            && self.right < width
            && self.bottom < height;
        fits.then(|| Area {
            left: self.left,
            top: self.top,
            width: self.right - self.left + 1,
            height: self.bottom - self.top + 1,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColorData {
    /// As-shot white balance levels in R, G, G, B order.
    pub as_shot: Option<[u16; 4]>,
    /// The camera's black level per channel (R, G, G, B) for this shot.
    pub black: Option<[u16; 4]>,
    pub specular_white: Option<u16>,
    /// dcraw's sRAW channel multipliers (R, G, B), in units of 1/1024.
    pub sraw_mul: Option<[u16; 3]>,
}

#[derive(Clone, Debug, Default)]
pub struct MakerNote {
    pub sensor: Option<SensorInfo>,
    pub color: Option<ColorData>,
    pub model_id: Option<u32>,
    pub firmware: Option<String>,
}

impl MakerNote {
    pub fn read(tiff: &Tiff<'_>, ifd: &Ifd) -> Result<Self, DecodeError> {
        let shorts = |tag| ifd.get(tag).map(|entry| tiff.unsigned(entry)).transpose();
        let sensor = shorts(SENSOR_INFO)?.and_then(|values| {
            let fields = SENSOR_FIELDS.map(|i| values.get(i).map(|&v| v as usize));
            let [width, height, left, top, right, bottom] = fields.map(|v| v.unwrap_or(0));
            fields.iter().all(Option::is_some).then_some(SensorInfo {
                width,
                height,
                left,
                top,
                right,
                bottom,
            })
        });
        let color = shorts(COLOR_DATA)?.map(|values| {
            let values: Vec<u16> = values.iter().map(|&v| v as u16).collect();
            color_data(&values)
        });
        let firmware = ifd.get(FIRMWARE).map(|entry| tiff.ascii(entry)).transpose()?;
        Ok(Self { sensor, color, model_id: tiff.get_u32(ifd, MODEL_ID)?, firmware })
    }

    /// Fills fields this note lacks from another one (CR3 spreads them out).
    pub fn merge(self, other: Self) -> Self {
        Self {
            sensor: self.sensor.or(other.sensor),
            color: self.color.or(other.color),
            model_id: self.model_id.or(other.model_id),
            firmware: self.firmware.or(other.firmware),
        }
    }
}

/// Decodes a ColorData block of a known version; unknown versions yield nothing.
pub fn color_data(values: &[u16]) -> ColorData {
    let Some(layout) = LAYOUTS.iter().find(|layout| layout.counts.contains(&values.len())) else {
        return ColorData::default();
    };
    let quad = |at: usize| values.get(at..at + 4).map(|v| [v[0], v[1], v[2], v[3]]);
    let as_shot = quad(layout.as_shot).filter(|levels| levels.iter().all(|&v| v > 0));
    let levels = layout.levels.and_then(|at| {
        let black = quad(at)?;
        let white = *values.get(at + SPECULAR_AFTER_BLACK)?;
        let (low, high) = (*black.iter().min()?, *black.iter().max()?);
        let plausible = low > 0
            && high - low <= MAX_BLACK_SPREAD
            && white > high.saturating_add(MIN_WHITE_ABOVE_BLACK);
        plausible.then_some((black, white))
    });
    ColorData {
        as_shot,
        black: levels.map(|(black, _)| black),
        specular_white: levels.map(|(_, white)| white),
        sraw_mul: sraw_multipliers(values, layout.as_shot),
    }
}

/// dcraw's search: the white balance entries after the as-shot one, five
/// shorts each, up to the one whose green is 1170 (or the last one tried).
fn sraw_multipliers(values: &[u16], as_shot: usize) -> Option<[u16; 3]> {
    let mut found = None;
    let mut at = as_shot + 4;
    // dcraw counts this limit in bytes against a length in shorts.
    let mut limit = 2 * as_shot + 18;
    while limit <= values.len() {
        let Some(entry) = values.get(at + 1..at + 5) else {
            break;
        };
        found = Some([entry[0], entry[1], entry[3]]);
        if entry[1] == SRAW_GREEN_MARK {
            break;
        }
        at += 5;
        limit += 10;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensor_borders_are_inclusive() {
        let info = SensorInfo { width: 100, height: 80, left: 10, top: 6, right: 97, bottom: 77 };
        assert_eq!(info.visible(100, 80), Some(Area { left: 10, top: 6, width: 88, height: 72 }));
        assert_eq!(info.visible(50, 80), None);
    }

    #[test]
    fn reads_color_data_version_7() {
        let mut values = vec![0_u16; 1312];
        values[0x3F..0x43].copy_from_slice(&[2000, 1024, 1024, 1500]);
        values[0x1F8..0x1FE].copy_from_slice(&[2047, 2047, 2048, 2047, 16383, 15000]);
        // The second white balance entry after the as-shot one carries the mark.
        values[0x3F + 4 + 5 + 1..][..4].copy_from_slice(&[
            900,
            SRAW_GREEN_MARK,
            SRAW_GREEN_MARK,
            500,
        ]);
        let data = color_data(&values);
        assert_eq!(data.as_shot, Some([2000, 1024, 1024, 1500]));
        assert_eq!(data.black, Some([2047, 2047, 2048, 2047]));
        assert_eq!(data.specular_white, Some(15000));
        assert_eq!(data.sraw_mul, Some([900, SRAW_GREEN_MARK, 500]));
    }

    #[test]
    fn rejects_implausible_levels_and_unknown_versions() {
        let mut values = vec![0_u16; 3656];
        values[0x55..0x59].copy_from_slice(&[1800, 1024, 1024, 1700]);
        let data = color_data(&values);
        assert_eq!(data.as_shot, Some([1800, 1024, 1024, 1700]));
        assert_eq!((data.black, data.specular_white), (None, None));
        assert_eq!(color_data(&[1; 1000]), ColorData::default());
    }
}
