//! Module: mirante_format_heic::hevc::deblock
//! Purpose: The deblocking filter (H.265 8.7.2) for intra pictures, where every
//! marked edge has boundary strength 2.
//! Created: 2026-11-16
//! Architecture: Runs over the whole picture after all slices are decoded:
//! vertical edges first, then horizontal ones, as the standard orders them.
//! ctu.rs marked the edges, QPs and unfiltered blocks in `Picture`.

use super::ctu::chroma_qp_from_index;
use super::params::Pps;
use super::picture::{Picture, PlaneBuffer, flags};
use super::slice::SliceHeader;

/// β' by Q (Table 8-12).
const BETA: [u8; 52] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18,
    20, 22, 24, 26, 28, 30, 32, 34, 36, 38, 40, 42, 44, 46, 48, 50, 52, 54, 56, 58, 60, 62, 64,
];
/// tC' by Q (Table 8-12).
const TC: [u8; 54] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 3,
    3, 3, 3, 4, 4, 4, 5, 5, 6, 6, 7, 8, 9, 10, 11, 13, 14, 16, 18, 20, 22, 24,
];
/// 2 * (bS - 1) for the intra boundary strength of 2.
const INTRA_TC_OFFSET: i32 = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    Vertical,
    Horizontal,
}

pub fn apply(picture: &mut Picture, slices: &[SliceHeader], pps: &Pps, chroma_format_idc: u32) {
    for direction in [Direction::Vertical, Direction::Horizontal] {
        filter_luma(picture, slices, direction);
        if chroma_format_idc != 0 {
            for component in 1..3 {
                let offset = if component == 1 { pps.cb_qp_offset } else { pps.cr_qp_offset };
                filter_chroma(picture, slices, direction, component, offset, chroma_format_idc);
            }
        }
    }
}

/// The marked edge units of one direction: (luma x, luma y, p unit, q unit).
fn edges(
    picture: &Picture,
    direction: Direction,
) -> impl Iterator<Item = (u32, u32, usize, usize)> + '_ {
    let flag = match direction {
        Direction::Vertical => flags::VERTICAL_EDGE,
        Direction::Horizontal => flags::HORIZONTAL_EDGE,
    };
    let width4 = picture.width4;
    picture.flags.iter().enumerate().filter(move |(_, f)| *f & flag != 0).map(move |(q, _)| {
        let (x, y) = ((q % width4) as u32 * 4, (q / width4) as u32 * 4);
        let p = if direction == Direction::Vertical { q - 1 } else { q - width4 };
        (x, y, p, q)
    })
}

fn filter_luma(picture: &mut Picture, slices: &[SliceHeader], direction: Direction) {
    let edges: Vec<_> = edges(picture, direction).collect();
    let plane = &mut picture.planes[0];
    let scale = 1 << (plane.bit_depth - 8);
    for (x, y, p, q) in edges {
        let slice = &slices
            [picture.ctb_slice[ctb_index(picture.log2_ctb, picture.width_in_ctbs, x, y)] as usize];
        let qp = (i32::from(picture.qp_y[p]) + i32::from(picture.qp_y[q]) + 1) >> 1;
        let beta = i32::from(BETA[(qp + slice.beta_offset).clamp(0, 51) as usize]) * scale;
        let tc =
            i32::from(TC[(qp + INTRA_TC_OFFSET + slice.tc_offset).clamp(0, 53) as usize]) * scale;
        if tc == 0 {
            continue;
        }
        let unfiltered = |unit: usize| picture.flags[unit] & flags::UNFILTERED != 0;
        let edge = Edge::new(plane, x as usize, y as usize, direction);
        luma_edge(plane, edge, beta, tc, unfiltered(p), unfiltered(q));
    }
}

fn filter_chroma(
    picture: &mut Picture,
    slices: &[SliceHeader],
    direction: Direction,
    component: usize,
    qp_offset: i32,
    chroma_format_idc: u32,
) {
    let (sx, sy) = picture.chroma_shift;
    // Chroma edges lie on an 8x8 chroma grid; each segment spans 4 chroma lines.
    let (across_shift, along_shift) = match direction {
        Direction::Vertical => (sx, sy),
        Direction::Horizontal => (sy, sx),
    };
    let edges: Vec<_> = edges(picture, direction)
        .filter(|&(x, y, _, _)| {
            let (across, along) = if direction == Direction::Vertical { (x, y) } else { (y, x) };
            (across >> across_shift) % 8 == 0 && along % (4 << along_shift) == 0
        })
        .collect();
    let plane = &mut picture.planes[component];
    let scale = 1 << (plane.bit_depth - 8);
    for (x, y, p, q) in edges {
        let slice = &slices
            [picture.ctb_slice[ctb_index(picture.log2_ctb, picture.width_in_ctbs, x, y)] as usize];
        let qpi = ((i32::from(picture.qp_y[p]) + i32::from(picture.qp_y[q]) + 1) >> 1) + qp_offset;
        let qpc = chroma_qp_from_index(qpi, chroma_format_idc);
        let tc =
            i32::from(TC[(qpc + INTRA_TC_OFFSET + slice.tc_offset).clamp(0, 53) as usize]) * scale;
        if tc == 0 {
            continue;
        }
        let unfiltered = |unit: usize| picture.flags[unit] & flags::UNFILTERED != 0;
        let edge = Edge::new(plane, (x >> sx) as usize, (y >> sy) as usize, direction);
        chroma_edge(plane, edge, tc, unfiltered(p), unfiltered(q));
    }
}

fn ctb_index(log2_ctb: u32, width_in_ctbs: u32, x: u32, y: u32) -> usize {
    ((y >> log2_ctb) * width_in_ctbs + (x >> log2_ctb)) as usize
}

/// A 4-line edge segment: q0 of line k sits at base + k * along; p_i at q0 - (i+1) * across.
#[derive(Clone, Copy)]
struct Edge {
    base: usize,
    across: usize,
    along: usize,
}

impl Edge {
    fn new(plane: &PlaneBuffer, x: usize, y: usize, direction: Direction) -> Self {
        let base = y * plane.width + x;
        match direction {
            Direction::Vertical => Self { base, across: 1, along: plane.width },
            Direction::Horizontal => Self { base, across: plane.width, along: 1 },
        }
    }

    fn p(self, line: usize, i: usize) -> usize {
        self.base + line * self.along - (i + 1) * self.across
    }

    fn q(self, line: usize, i: usize) -> usize {
        self.base + line * self.along + i * self.across
    }
}

/// 8.7.2.5.3 decisions and 8.7.2.5.7 filtering for one luma edge segment.
fn luma_edge(plane: &mut PlaneBuffer, edge: Edge, beta: i32, tc: i32, no_p: bool, no_q: bool) {
    let max = (1 << plane.bit_depth) - 1;
    let s = &mut plane.samples;
    let at = |s: &[u16], i: usize| i32::from(s[i]);
    let second_derivative = |s: &[u16], line: usize, p_side: bool| {
        let pick = |i| if p_side { edge.p(line, i) } else { edge.q(line, i) };
        (at(s, pick(2)) - 2 * at(s, pick(1)) + at(s, pick(0))).abs()
    };
    let (dp0, dp3) = (second_derivative(s, 0, true), second_derivative(s, 3, true));
    let (dq0, dq3) = (second_derivative(s, 0, false), second_derivative(s, 3, false));
    if dp0 + dq0 + dp3 + dq3 >= beta {
        return;
    }
    let strong_line = |s: &[u16], line: usize, dpq: i32| {
        let (p0, p3) = (at(s, edge.p(line, 0)), at(s, edge.p(line, 3)));
        let (q0, q3) = (at(s, edge.q(line, 0)), at(s, edge.q(line, 3)));
        2 * dpq < (beta >> 2)
            && (p3 - p0).abs() + (q0 - q3).abs() < (beta >> 3)
            && (p0 - q0).abs() < ((5 * tc + 1) >> 1)
    };
    let strong = strong_line(s, 0, dp0 + dq0) && strong_line(s, 3, dp3 + dq3);
    let side_threshold = (beta + (beta >> 1)) >> 3;
    let (extend_p, extend_q) = (dp0 + dp3 < side_threshold, dq0 + dq3 < side_threshold);

    for line in 0..4 {
        let p: [i32; 4] = std::array::from_fn(|i| at(s, edge.p(line, i)));
        let q: [i32; 4] = std::array::from_fn(|i| at(s, edge.q(line, i)));
        if strong {
            let clip =
                |value: i32, original: i32| value.clamp(original - 2 * tc, original + 2 * tc);
            if !no_p {
                s[edge.p(line, 0)] =
                    clip((p[2] + 2 * p[1] + 2 * p[0] + 2 * q[0] + q[1] + 4) >> 3, p[0]) as u16;
                s[edge.p(line, 1)] = clip((p[2] + p[1] + p[0] + q[0] + 2) >> 2, p[1]) as u16;
                s[edge.p(line, 2)] =
                    clip((2 * p[3] + 3 * p[2] + p[1] + p[0] + q[0] + 4) >> 3, p[2]) as u16;
            }
            if !no_q {
                s[edge.q(line, 0)] =
                    clip((p[1] + 2 * p[0] + 2 * q[0] + 2 * q[1] + q[2] + 4) >> 3, q[0]) as u16;
                s[edge.q(line, 1)] = clip((p[0] + q[0] + q[1] + q[2] + 2) >> 2, q[1]) as u16;
                s[edge.q(line, 2)] =
                    clip((p[0] + q[0] + q[1] + 3 * q[2] + 2 * q[3] + 4) >> 3, q[2]) as u16;
            }
            continue;
        }
        let delta = (9 * (q[0] - p[0]) - 3 * (q[1] - p[1]) + 8) >> 4;
        if delta.abs() >= tc * 10 {
            continue;
        }
        let delta = delta.clamp(-tc, tc);
        if !no_p {
            s[edge.p(line, 0)] = (p[0] + delta).clamp(0, max) as u16;
            if extend_p {
                let dp =
                    ((((p[2] + p[0] + 1) >> 1) - p[1] + delta) >> 1).clamp(-(tc >> 1), tc >> 1);
                s[edge.p(line, 1)] = (p[1] + dp).clamp(0, max) as u16;
            }
        }
        if !no_q {
            s[edge.q(line, 0)] = (q[0] - delta).clamp(0, max) as u16;
            if extend_q {
                let dq =
                    ((((q[2] + q[0] + 1) >> 1) - q[1] - delta) >> 1).clamp(-(tc >> 1), tc >> 1);
                s[edge.q(line, 1)] = (q[1] + dq).clamp(0, max) as u16;
            }
        }
    }
}

/// 8.7.2.5.5: the chroma filter changes one sample on each side.
fn chroma_edge(plane: &mut PlaneBuffer, edge: Edge, tc: i32, no_p: bool, no_q: bool) {
    let max = (1 << plane.bit_depth) - 1;
    let s = &mut plane.samples;
    for line in 0..4 {
        let (p0, p1) = (i32::from(s[edge.p(line, 0)]), i32::from(s[edge.p(line, 1)]));
        let (q0, q1) = (i32::from(s[edge.q(line, 0)]), i32::from(s[edge.q(line, 1)]));
        let delta = ((((q0 - p0) << 2) + p1 - q1 + 4) >> 3).clamp(-tc, tc);
        if !no_p {
            s[edge.p(line, 0)] = (p0 + delta).clamp(0, max) as u16;
        }
        if !no_q {
            s[edge.q(line, 0)] = (q0 - delta).clamp(0, max) as u16;
        }
    }
}
