//! Module: mirante_format_heic::hevc::sao
//! Purpose: Sample adaptive offset (H.265 8.7.3): band and edge offsets per CTB.
//! Created: 2026-11-16
//! Architecture: Runs after deblocking on a copy of the deblocked plane, so every
//! CTB sees unmodified neighbours; ctu.rs stored the per-CTB parameters.

use super::params::Pps;
use super::picture::{NOT_DECODED, Picture, flags};
use super::slice::SliceHeader;

const BAND: u8 = 1;
/// Neighbour offsets (dx, dy) of the two samples compared by each edge class.
const EDGE_NEIGHBOURS: [[(i32, i32); 2]; 4] =
    [[(-1, 0), (1, 0)], [(0, -1), (0, 1)], [(-1, -1), (1, 1)], [(1, -1), (-1, 1)]];

pub fn apply(picture: &mut Picture, slices: &[SliceHeader], pps: &Pps) {
    for component in 0..picture.planes.len() {
        if picture.sao.iter().all(|ctb| ctb[component].kind == 0) {
            continue;
        }
        let source = picture.planes[component].samples.clone();
        let ctbs = (picture.width_in_ctbs * picture.height_in_ctbs) as usize;
        for rs in 0..ctbs {
            if picture.sao[rs][component].kind != 0 && picture.ctb_slice_address[rs] != NOT_DECODED
            {
                apply_ctb(picture, slices, pps, component, rs, &source);
            }
        }
    }
}

/// Whether edge offsets may look into each of the 3x3 CTBs around `rs`.
fn neighbour_access(
    picture: &Picture,
    slices: &[SliceHeader],
    pps: &Pps,
    rs: usize,
) -> [[bool; 3]; 3] {
    let width = picture.width_in_ctbs as i32;
    let (x, y) = (rs as i32 % width, rs as i32 / width);
    let tiles = &picture.tiles;
    std::array::from_fn(|row| {
        std::array::from_fn(|column| {
            let (nx, ny) = (x + column as i32 - 1, y + row as i32 - 1);
            if nx < 0 || ny < 0 || nx >= width || ny >= picture.height_in_ctbs as i32 {
                return false;
            }
            let other = (ny * width + nx) as usize;
            if picture.ctb_slice_address[other] == NOT_DECODED {
                return false;
            }
            if tiles.tile_id[other] != tiles.tile_id[rs] && !pps.loop_filter_across_tiles {
                return false;
            }
            if picture.ctb_slice_address[other] == picture.ctb_slice_address[rs] {
                return true;
            }
            // The slice decoded later decides whether filtering may cross the boundary.
            let later = if tiles.rs_to_ts[other] > tiles.rs_to_ts[rs] { other } else { rs };
            slices[picture.ctb_slice[later] as usize].loop_filter_across_slices
        })
    })
}

fn apply_ctb(
    picture: &mut Picture,
    slices: &[SliceHeader],
    pps: &Pps,
    component: usize,
    rs: usize,
    source: &[u16],
) {
    let params = picture.sao[rs][component];
    let (sx, sy) = if component == 0 { (0, 0) } else { picture.chroma_shift };
    let width_in_ctbs = picture.width_in_ctbs as usize;
    let ctb_size = 1_usize << picture.log2_ctb;
    let (plane_width, plane_height) =
        (picture.planes[component].width, picture.planes[component].height);
    let x0 = ((rs % width_in_ctbs) * ctb_size) >> sx;
    let y0 = ((rs / width_in_ctbs) * ctb_size) >> sy;
    let x1 = (x0 + (ctb_size >> sx)).min(plane_width);
    let y1 = (y0 + (ctb_size >> sy)).min(plane_height);
    let access = neighbour_access(picture, slices, pps, rs);
    let max = (1_i32 << picture.planes[component].bit_depth) - 1;
    let band_shift = picture.planes[component].bit_depth - 5;
    let mut band_table = [0_usize; 32];
    for k in 0..4 {
        band_table[(k + usize::from(params.detail)) & 31] = k + 1;
    }
    let unfiltered_anywhere = (y0 << sy..y1 << sy).step_by(4).any(|y| {
        (x0 << sx..x1 << sx)
            .step_by(4)
            .any(|x| picture.flags[picture.unit(x as u32, y as u32)] & flags::UNFILTERED != 0)
    });
    let neighbours = EDGE_NEIGHBOURS[usize::from(params.detail) & 3];

    for y in y0..y1 {
        for x in x0..x1 {
            if unfiltered_anywhere
                && picture.flags[picture.unit((x << sx) as u32, (y << sy) as u32)]
                    & flags::UNFILTERED
                    != 0
            {
                continue;
            }
            let sample = i32::from(source[y * plane_width + x]);
            let class = if params.kind == BAND {
                band_table[(sample >> band_shift) as usize]
            } else {
                let mut sum = 0;
                let mut usable = true;
                for (dx, dy) in neighbours {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    let column = region(nx, x0, x1);
                    let row = region(ny, y0, y1);
                    if nx < 0 || ny < 0 || !access[row][column] {
                        usable = false;
                        break;
                    }
                    let neighbour = i32::from(source[ny as usize * plane_width + nx as usize]);
                    sum += (sample - neighbour).signum();
                }
                if !usable {
                    continue;
                }
                match sum {
                    -2 => 1,
                    -1 => 2,
                    0 => 0,
                    1 => 3,
                    _ => 4,
                }
            };
            if class != 0 {
                let value = (sample + i32::from(params.offsets[class - 1])).clamp(0, max);
                picture.planes[component].samples[y * plane_width + x] = value as u16;
            }
        }
    }
}

/// 0, 1 or 2 for a coordinate before, inside or after the CTB's range.
fn region(value: i32, start: usize, end: usize) -> usize {
    if value < start as i32 {
        0
    } else if value >= end as i32 {
        2
    } else {
        1
    }
}
