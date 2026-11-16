//! Module: mirante_raw::demosaic
//! Purpose: Rebuilds full RGB from a colour filter array mosaic.
//! Created: 2026-11-16
//! Architecture: 2x2 Bayer patterns use Malvar-He-Cutler gradient-corrected
//! interpolation. Other patterns (X-Trans 6x6) interpolate green along the
//! smoothest directions, then red and blue as edge-weighted colour differences.
//! Borders mirror; colours are always looked up at the mirrored photosite.

use mirante_core::{DecodeError, Rgba, parallel};
use std::borrow::Cow;

pub const RED: u8 = 0;
pub const GREEN: u8 = 1;
pub const BLUE: u8 = 2;
/// Largest repeat pattern we accept; X-Trans is 6x6.
const MAX_PATTERN_SIDE: usize = 8;
/// How far the directional green search looks for each of its two greens.
const GREEN_REACH: isize = 3;
/// Malvar-He-Cutler's 5x5 kernels reach two sites from the centre.
const MALVAR_REACH: usize = 2;
/// Mirroring must cover two green searches in a row.
const MARGIN: usize = 2 * GREEN_REACH as usize;
/// Window radii for the colour differences; the wider one only when needed.
const DIFFERENCE_RADII: [isize; 2] = [2, 3];
/// Keeps gradient weights finite on flat areas (values are 0..1 of white).
const GRADIENT_FLOOR: f32 = 1e-3;
/// Horizontal, vertical and the two diagonals.
const DIRECTIONS: [(isize, isize); 4] = [(1, 0), (0, 1), (1, 1), (1, -1)];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CfaPattern {
    width: usize,
    height: usize,
    colors: Vec<u8>,
}

impl CfaPattern {
    /// `colors` is row-major, with RED, GREEN and BLUE values.
    pub fn new(width: usize, height: usize, colors: Vec<u8>) -> Result<Self, DecodeError> {
        let sides =
            (1..=MAX_PATTERN_SIDE).contains(&width) && (1..=MAX_PATTERN_SIDE).contains(&height);
        if !sides || colors.len() != width * height {
            return Err(DecodeError::unsupported("CFA repeat pattern size"));
        }
        if colors.iter().any(|&c| c > BLUE)
            || [RED, GREEN, BLUE].iter().any(|c| !colors.contains(c))
        {
            return Err(DecodeError::unsupported("CFA colours other than red, green and blue"));
        }
        Ok(Self { width, height, colors })
    }

    pub fn color_at(&self, x: usize, y: usize) -> u8 {
        self.colors[(y % self.height) * self.width + x % self.width]
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// The same pattern seen from (left, top), for a crop of the sensor.
    pub fn shifted(&self, left: usize, top: usize) -> Self {
        let colors = (0..self.width * self.height)
            .map(|i| self.color_at(left + i % self.width, top + i / self.width))
            .collect();
        Self { colors, ..*self }
    }

    /// Two greens on one diagonal, red and blue on the other.
    fn is_bayer(&self) -> bool {
        let mut sorted = self.colors.clone();
        sorted.sort_unstable();
        let diagonal_pair = self.color_at(0, 0) == self.color_at(1, 1)
            || self.color_at(1, 0) == self.color_at(0, 1);
        (self.width, self.height) == (2, 2) && sorted == [RED, GREEN, GREEN, BLUE] && diagonal_pair
    }
}

/// A single-channel sensor image with values normalised to 0..1.
pub struct Mosaic<'a> {
    pub width: usize,
    pub height: usize,
    pub values: &'a [f32],
    pub pattern: &'a CfaPattern,
}

/// Index of a mirrored coordinate, for -MARGIN..length+MARGIN.
fn mirror_table(length: usize) -> Vec<usize> {
    let last = length as isize - 1;
    (-(MARGIN as isize)..length as isize + MARGIN as isize)
        .map(|i| {
            let reflected = if i < 0 {
                -i
            } else if i > last {
                2 * last - i
            } else {
                i
            };
            reflected.clamp(0, last) as usize
        })
        .collect()
}

/// Mirrored access to the photosites around a position.
struct Neighbours<'a> {
    mosaic: &'a Mosaic<'a>,
    /// Mirrored column (row) of each virtual one, and its CFA pattern phase.
    columns: Vec<(usize, usize)>,
    rows: Vec<(usize, usize)>,
}

impl<'a> Neighbours<'a> {
    fn new(mosaic: &'a Mosaic<'a>) -> Self {
        let phased = |length: usize, period: usize| {
            mirror_table(length).into_iter().map(|i| (i, i % period)).collect()
        };
        Self {
            mosaic,
            columns: phased(mosaic.width, mosaic.pattern.width),
            rows: phased(mosaic.height, mosaic.pattern.height),
        }
    }

    /// Plane index and colour of the photosite at (x + dx, y + dy), mirrored at
    /// the borders.
    fn site(&self, x: usize, y: usize, dx: isize, dy: isize) -> (usize, u8) {
        let (column, column_phase) = self.columns[(x as isize + dx + MARGIN as isize) as usize];
        let (row, row_phase) = self.rows[(y as isize + dy + MARGIN as isize) as usize];
        let pattern = self.mosaic.pattern;
        (row * self.mosaic.width + column, pattern.colors[row_phase * pattern.width + column_phase])
    }

    fn value(&self, x: usize, y: usize, dx: isize, dy: isize) -> f32 {
        self.mosaic.values[self.site(x, y, dx, dy).0]
    }
}

impl Mosaic<'_> {
    /// Demosaics the rectangle at (left, top) of the given size into opaque RGBA.
    /// The rectangle must lie inside the mosaic.
    pub fn demosaic(&self, left: usize, top: usize, width: usize, height: usize) -> Vec<Rgba> {
        let around = Neighbours::new(self);
        // Zeroed memory is mapped lazily, so the threads below fault it in in parallel.
        let mut out = vec![[0.0; 4]; width * height];
        if self.pattern.is_bayer() {
            parallel::for_each_indexed_chunk(&mut out, |first, part| {
                for (offset, pixel) in part.iter_mut().enumerate() {
                    let (x, y) = (left + (first + offset) % width, top + (first + offset) / width);
                    let interior = x >= MALVAR_REACH
                        && y >= MALVAR_REACH
                        && x + MALVAR_REACH < self.width
                        && y + MALVAR_REACH < self.height;
                    let rgb = if interior {
                        let (base, stride) = ((y * self.width + x) as isize, self.width as isize);
                        self.malvar(x, y, |dx, dy| self.values[(base + dy * stride + dx) as usize])
                    } else {
                        self.malvar(x, y, |dx, dy| around.value(x, y, dx, dy))
                    };
                    *pixel = [rgb[0], rgb[1], rgb[2], 1.0];
                }
            });
            return out;
        }
        // Interior sites see the periodic pattern, so their taps are found once
        // per pattern phase; sites near the border find theirs through mirroring.
        let pattern = self.pattern;
        let plan: Vec<Taps> = (0..pattern.width * pattern.height)
            .map(|phase| {
                let (px, py) = ((phase % pattern.width) as isize, (phase / pattern.width) as isize);
                Taps::find(|dx, dy| {
                    let x = (px + dx).rem_euclid(pattern.width as isize) as usize;
                    pattern.color_at(x, (py + dy).rem_euclid(pattern.height as isize) as usize)
                })
            })
            .collect();
        // Sites near the border find their taps through mirroring.
        let site = |x: usize, y: usize| {
            let interior =
                x >= MARGIN && y >= MARGIN && x + MARGIN < self.width && y + MARGIN < self.height;
            if interior {
                let phase = (y % pattern.height) * pattern.width + x % pattern.width;
                let centre = (y * self.width + x) as isize;
                (Cow::Borrowed(&plan[phase]), Reach::Direct { centre, stride: self.width as isize })
            } else {
                let taps = Taps::find(|dx, dy| around.site(x, y, dx, dy).1);
                (Cow::Owned(taps), Reach::Mirrored { around: &around, x, y })
            }
        };
        let mut green = vec![0.0; self.values.len()];
        parallel::for_each_indexed_chunk(&mut green, |first, part| {
            for (offset, value) in part.iter_mut().enumerate() {
                let (x, y) = ((first + offset) % self.width, (first + offset) / self.width);
                let (taps, reach) = site(x, y);
                *value = taps.green(self.values, &reach);
            }
        });
        parallel::for_each_indexed_chunk(&mut out, |first, part| {
            for (offset, pixel) in part.iter_mut().enumerate() {
                let (x, y) = (left + (first + offset) % width, top + (first + offset) / width);
                let own = pattern.color_at(x, y);
                let (taps, reach) = site(x, y);
                let rgb = taps.colours(own, self.values, &green, &reach);
                *pixel = [rgb[0], rgb[1], rgb[2], 1.0];
            }
        });
        median_colour_differences(&mut out, width, height);
        out
    }

    /// Malvar, He and Cutler (2004), "High-quality linear interpolation for
    /// demosaicing of Bayer-patterned color images"; weights are in eighths.
    fn malvar(&self, x: usize, y: usize, p: impl Fn(isize, isize) -> f32) -> [f32; 3] {
        let centre = p(0, 0);
        let cross = p(-1, 0) + p(1, 0) + p(0, -1) + p(0, 1);
        let axial = p(-2, 0) + p(2, 0) + p(0, -2) + p(0, 2);
        let diagonal = p(-1, -1) + p(1, -1) + p(-1, 1) + p(1, 1);
        let horizontal = (p(-1, 0) + p(1, 0), p(-2, 0) + p(2, 0));
        let vertical = (p(0, -1) + p(0, 1), p(0, -2) + p(0, 2));
        let eighths = |sum: f32| sum / 8.0;

        let color = self.pattern.color_at(x, y);
        if color == GREEN {
            // Green with red or blue neighbours along the row (h) or the column (v).
            let along_row = eighths(
                5.0 * centre + 4.0 * horizontal.0 - diagonal - horizontal.1 + 0.5 * vertical.1,
            );
            let along_column = eighths(
                5.0 * centre + 4.0 * vertical.0 - diagonal - vertical.1 + 0.5 * horizontal.1,
            );
            return if self.pattern.color_at(x + 1, y) == RED {
                [along_row, centre, along_column]
            } else {
                [along_column, centre, along_row]
            };
        }
        let green = eighths(4.0 * centre + 2.0 * cross - axial);
        let opposite = eighths(6.0 * centre + 2.0 * diagonal - 1.5 * axial);
        if color == RED { [centre, green, opposite] } else { [opposite, green, centre] }
    }
}

/// Compare-and-swap steps after which the middle of nine values is in place
/// (Paeth's median-of-9 network).
#[rustfmt::skip]
const MEDIAN_OF_9: [(usize, usize); 19] = [
    (1, 2), (4, 5), (7, 8), (0, 1), (3, 4), (6, 7), (1, 2), (4, 5), (7, 8), (0, 3),
    (5, 8), (4, 7), (3, 6), (1, 4), (2, 5), (4, 7), (4, 2), (6, 4), (4, 2),
];

fn median_of_9(mut values: [f32; 9]) -> f32 {
    for (a, b) in MEDIAN_OF_9 {
        (values[a], values[b]) = (values[a].min(values[b]), values[a].max(values[b]));
    }
    values[4]
}

/// Replaces red and blue minus green by their 3x3 medians, as dcraw's median
/// filter does: removes isolated false-colour specks around fine bright detail.
fn median_colour_differences(pixels: &mut [Rgba], width: usize, height: usize) {
    let mut differences = vec![[0.0_f32; 2]; pixels.len()];
    parallel::for_each_indexed_chunk(&mut differences, |first, part| {
        for (difference, p) in part.iter_mut().zip(&pixels[first..]) {
            *difference = [p[0] - p[1], p[2] - p[1]];
        }
    });
    parallel::for_each_indexed_chunk(pixels, |first, part| {
        for (offset, pixel) in part.iter_mut().enumerate() {
            let (x, y) = ((first + offset) % width, (first + offset) / width);
            let columns = [x.saturating_sub(1), x, (x + 1).min(width - 1)];
            let rows = [y.saturating_sub(1), y, (y + 1).min(height - 1)];
            for (channel, k) in [(0, 0), (2, 1)] {
                let window: [f32; 9] =
                    std::array::from_fn(|i| differences[rows[i / 3] * width + columns[i % 3]][k]);
                pixel[channel] = pixel[1] + median_of_9(window);
            }
        }
    });
}

/// How a site reaches its neighbours: directly inside the mosaic, mirrored
/// near its border.
enum Reach<'a> {
    Direct { centre: isize, stride: isize },
    Mirrored { around: &'a Neighbours<'a>, x: usize, y: usize },
}

impl Reach<'_> {
    fn index(&self, dx: isize, dy: isize) -> usize {
        match *self {
            Self::Direct { centre, stride } => (centre + dy * stride + dx) as usize,
            Self::Mirrored { around, x, y } => around.site(x, y, dx, dy).0,
        }
    }
}

/// Steps to the nearest greens ahead and behind along one direction, and to the
/// next greens beyond them (0 when there is none within reach).
#[derive(Clone, Copy, Debug)]
struct GreenLine {
    dx: isize,
    dy: isize,
    ahead: isize,
    behind: isize,
    far_ahead: isize,
    far_behind: isize,
}

/// The neighbours a non-Bayer site interpolates from; they depend only on the
/// CFA colours around it.
#[derive(Clone)]
struct Taps {
    /// Directions with a green on both sides; empty at green sites.
    green: Vec<GreenLine>,
    /// Same-colour sites (dx, dy, squared distance) for missing red and blue.
    colours: [Vec<(isize, isize, f32)>; 2],
}

impl Taps {
    fn find(color_of: impl Fn(isize, isize) -> u8) -> Self {
        let own = color_of(0, 0);
        let nearest = |dx: isize, dy: isize, start: isize| {
            (start + 1..=start + GREEN_REACH).find(|&step| color_of(dx * step, dy * step) == GREEN)
        };
        let lines = DIRECTIONS.iter().filter_map(|&(dx, dy)| {
            let (ahead, behind) = (nearest(dx, dy, 0)?, nearest(-dx, -dy, 0)?);
            let far_ahead = nearest(dx, dy, ahead).unwrap_or(0);
            let far_behind = nearest(-dx, -dy, behind).unwrap_or(0);
            Some(GreenLine { dx, dy, ahead, behind, far_ahead, far_behind })
        });
        let green = if own == GREEN { Vec::new() } else { lines.collect() };
        let colours = [RED, BLUE].map(|color| {
            let sites = |radius: isize| {
                let mut found = Vec::new();
                for dy in -radius..=radius {
                    for dx in -radius..=radius {
                        if color_of(dx, dy) == color {
                            found.push((dx, dy, (dx * dx + dy * dy) as f32));
                        }
                    }
                }
                found
            };
            let wanted = color != own;
            let mut radii = DIFFERENCE_RADII.iter();
            let found = radii.find_map(|&r| Some(sites(r)).filter(|f| !f.is_empty()));
            if wanted { found.unwrap_or_default() } else { Vec::new() }
        });
        Self { green, colours }
    }

    /// Green at a red or blue site: per direction, the distance-weighted mean of
    /// the greens on either side; directions are weighted by their smoothness.
    fn green(&self, values: &[f32], reach: &Reach<'_>) -> f32 {
        let index_of = |dx, dy| reach.index(dx, dy);
        let (mut sum, mut weights) = (0.0, 0.0);
        for line in &self.green {
            let at = |step: isize| values[index_of(line.dx * step, line.dy * step)];
            let (near_ahead, near_behind) = (at(line.ahead), at(-line.behind));
            let (a, b) = (line.ahead as f32, line.behind as f32);
            let estimate = (near_ahead * b + near_behind * a) / (a + b);
            // Smoothness from the two greens and, where present, the next ones out.
            let mut gradient = (near_ahead - near_behind).abs();
            if line.far_ahead > 0 {
                gradient += 0.5 * (at(line.far_ahead) - near_ahead).abs();
            }
            if line.far_behind > 0 {
                gradient += 0.5 * (at(-line.far_behind) - near_behind).abs();
            }
            let weight = 1.0 / (gradient + GRADIENT_FLOOR).powi(2);
            sum += weight * estimate;
            weights += weight;
        }
        if weights > 0.0 { sum / weights } else { values[index_of(0, 0)] }
    }

    /// Red and blue as green plus the colour difference of nearby same-colour
    /// sites, favouring close sites whose green resembles this one's.
    fn colours(&self, own: u8, values: &[f32], green: &[f32], reach: &Reach<'_>) -> [f32; 3] {
        let index_of = |dx, dy| reach.index(dx, dy);
        let centre = index_of(0, 0);
        let here = green[centre];
        let mut rgb = [here; 3];
        rgb[usize::from(own)] = values[centre];
        for (color, sites) in [RED, BLUE].into_iter().zip(&self.colours) {
            let (mut sum, mut weights) = (0.0, 0.0);
            for &(dx, dy, distance) in sites {
                let index = index_of(dx, dy);
                let weight = 1.0 / (distance * ((green[index] - here).abs() + GRADIENT_FLOOR));
                sum += weight * (values[index] - green[index]);
                weights += weight;
            }
            if weights > 0.0 {
                rgb[usize::from(color)] = here + sum / weights;
            }
        }
        rgb
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RGGB: [u8; 4] = [RED, GREEN, GREEN, BLUE];

    fn xtrans() -> CfaPattern {
        let colors = b"GGRGGBGGBGGRBRGRBGGGBGGRGGRGGBRBGBRG"
            .iter()
            .map(|c| match c {
                b'R' => RED,
                b'G' => GREEN,
                _ => BLUE,
            })
            .collect();
        CfaPattern::new(6, 6, colors).unwrap()
    }

    /// A mosaic of an image as each photosite would record it.
    fn sample(
        pattern: &CfaPattern,
        width: usize,
        height: usize,
        rgb: impl Fn(usize, usize) -> [f32; 3],
    ) -> Vec<f32> {
        (0..width * height)
            .map(|i| {
                let (x, y) = (i % width, i / width);
                rgb(x, y)[usize::from(pattern.color_at(x, y))]
            })
            .collect()
    }

    fn assert_flat(pattern: CfaPattern, rgb: [f32; 3]) {
        let (width, height) = (13, 11);
        let values = sample(&pattern, width, height, |_, _| rgb);
        let mosaic = Mosaic { width, height, values: &values, pattern: &pattern };
        for pixel in mosaic.demosaic(0, 0, width, height) {
            assert!(pixel[..3].iter().zip(rgb).all(|(a, b)| (a - b).abs() < 1e-5), "{pixel:?}");
        }
    }

    const BAYER_PHASES: [[u8; 4]; 4] =
        [RGGB, [GREEN, RED, BLUE, GREEN], [GREEN, BLUE, RED, GREEN], [BLUE, GREEN, GREEN, RED]];

    #[test]
    fn flat_colours_survive_every_bayer_phase() {
        for phase in BAYER_PHASES {
            assert_flat(CfaPattern::new(2, 2, phase.to_vec()).unwrap(), [0.8, 0.4, 0.1]);
        }
    }

    #[test]
    fn linear_ramps_are_reconstructed_exactly() {
        // Each kernel is exact on linear signals, so any mixed-up tap shows here.
        let (width, height) = (10, 10);
        let ramp = |x: usize, y: usize| {
            let (x, y) = (x as f32, y as f32);
            [0.1 + 0.02 * x, 0.2 + 0.01 * y, 0.3 + 0.01 * x + 0.015 * y]
        };
        for phase in BAYER_PHASES {
            let pattern = CfaPattern::new(2, 2, phase.to_vec()).unwrap();
            let values = sample(&pattern, width, height, ramp);
            let mosaic = Mosaic { width, height, values: &values, pattern: &pattern };
            let inner = mosaic.demosaic(2, 2, width - 4, height - 4);
            for (i, pixel) in inner.iter().enumerate() {
                let expected = ramp(2 + i % (width - 4), 2 + i / (width - 4));
                assert!(
                    pixel.iter().zip(expected).all(|(a, b)| (a - b).abs() < 1e-5),
                    "{phase:?} {i} {pixel:?} {expected:?}"
                );
            }
        }
    }

    #[test]
    fn flat_colours_survive_every_xtrans_phase() {
        let pattern = xtrans();
        for phase in 0..6 {
            assert_flat(pattern.shifted(phase, 5 - phase), [0.2, 0.6, 0.9]);
        }
    }

    #[test]
    fn xtrans_keeps_a_vertical_edge_sharp_and_colourless() {
        let pattern = xtrans();
        let (width, height) = (24, 18);
        let edge = |x: usize, _| if x < 12 { [0.1; 3] } else { [0.7; 3] };
        let values = sample(&pattern, width, height, edge);
        let mosaic = Mosaic { width, height, values: &values, pattern: &pattern };
        let out = mosaic.demosaic(0, 0, width, height);
        for (i, pixel) in out.iter().enumerate() {
            let x = i % width;
            let expected = edge(x, 0)[0];
            // Away from the edge column itself the result must be exact and grey.
            if !(11..=12).contains(&x) {
                assert!(pixel[..3].iter().all(|v| (v - expected).abs() < 0.02), "{x}: {pixel:?}");
            }
        }
    }

    #[test]
    fn crops_while_demosaicing() {
        let pattern = CfaPattern::new(2, 2, RGGB.to_vec()).unwrap();
        let values = sample(&pattern, 6, 4, |_, _| [0.5; 3]);
        let mosaic = Mosaic { width: 6, height: 4, values: &values, pattern: &pattern };
        assert_eq!(mosaic.demosaic(1, 1, 3, 2).len(), 6);
    }

    #[test]
    fn median_network_finds_the_middle_value() {
        let mut seed = 1_u32;
        for _ in 0..200 {
            let values: [f32; 9] = std::array::from_fn(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (seed >> 16) as f32
            });
            let mut sorted = values;
            sorted.sort_by(f32::total_cmp);
            assert_eq!(median_of_9(values), sorted[4]);
        }
    }

    #[test]
    fn shifting_moves_the_pattern_origin() {
        let pattern = CfaPattern::new(2, 2, RGGB.to_vec()).unwrap();
        assert_eq!(pattern.shifted(1, 1).color_at(0, 0), BLUE);
        assert_eq!(pattern.shifted(1, 0).color_at(0, 0), GREEN);
    }

    #[test]
    fn rejects_non_rgb_patterns() {
        assert!(CfaPattern::new(2, 2, vec![RED, GREEN, GREEN, GREEN]).is_err());
        assert!(CfaPattern::new(2, 2, vec![RED, GREEN, 3, BLUE]).is_err());
        assert!(CfaPattern::new(9, 2, vec![GREEN; 18]).is_err());
        let stripes = CfaPattern::new(2, 2, vec![GREEN, GREEN, RED, BLUE]).unwrap();
        assert!(!stripes.is_bayer());
        assert!(BAYER_PHASES.iter().all(|p| CfaPattern::new(2, 2, p.to_vec()).unwrap().is_bayer()));
    }
}
