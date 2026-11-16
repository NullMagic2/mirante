//! Module: mirante_format_heic::hevc::ctu
//! Purpose: slice_segment_data() for intra slices (H.265 7.3.8): CTU order with
//! tiles and WPP, SAO syntax, coding quadtree, intra CUs with MPM derivation,
//! PCM, transform tree, QP derivation, and reconstruction of every block.
//! Created: 2026-11-16
//! Architecture: One `SliceDecoder` per slice segment writes into the shared
//! `Picture`; `Carry` hands CABAC state between segments (WPP rows, dependent
//! slices). Residuals come from residual.rs/transform.rs, predictions from intra.rs.

use super::bitstream::BitReader;
use super::cabac::{Cabac, ContextSet, ctx};
use super::intra::{self, DC, IntraOptions};
use super::params::{Pps, Sps};
use super::picture::{NOT_DECODED, Picture, SaoComponent, flags};
use super::residual::{self, ResidualParams};
use super::slice::SliceHeader;
use super::transform::{self, BlockParams, MAX_COEFFS};
use mirante_core::DecodeError;

/// Mode mapping for 4:2:2 chroma (Table 8-3).
const MODE_422: [u8; 35] = [
    0, 1, 2, 2, 2, 2, 3, 5, 7, 8, 10, 12, 13, 15, 17, 18, 19, 20, 21, 22, 23, 23, 24, 24, 25, 25,
    26, 27, 27, 28, 28, 29, 29, 30, 31,
];
/// QpC for qPi 30..=43 in 4:2:0 (Table 8-10).
const CHROMA_QP_420: [i32; 14] = [29, 30, 31, 32, 33, 33, 34, 34, 35, 35, 36, 36, 37, 37];
const SAO_BAND: u8 = 1;
const SAO_EDGE: u8 = 2;
const CU_QP_DELTA_PREFIX: u32 = 5;
const MAX_GOLOMB_PREFIX: u32 = 31;
const REFERENCE_LINE: usize = 4 * 32 + 1;

/// CABAC state handed from one slice segment to the next within a picture.
#[derive(Default)]
pub struct Carry {
    /// Stored after the second CTB of a row for WPP (9.3.2.4).
    wpp: Option<ContextSet>,
    /// Stored at the end of a segment for a dependent segment, with qPY_PREV.
    dependent: Option<(ContextSet, i32)>,
}

pub fn chroma_qp_from_index(qpi: i32, chroma_format_idc: u32) -> i32 {
    match (chroma_format_idc, qpi) {
        (1, ..30) => qpi,
        (1, 30..=43) => CHROMA_QP_420[(qpi - 30) as usize],
        (1, _) => qpi - 6,
        _ => qpi.min(51),
    }
}

/// The parsed intra coding unit, as the transform tree needs it.
struct CodingUnit {
    x: u32,
    y: u32,
    log2_size: u32,
    bypass: bool,
    intra_split: bool,
    /// IntraPredModeC per chroma prediction block (one, or four for 4:4:4 NxN).
    chroma_modes: [u8; 4],
    /// intra_chroma_pred_mode was 4 (DM), which enables cross-component prediction.
    chroma_dm: [bool; 4],
}

impl CodingUnit {
    /// Index of the chroma prediction block covering luma (x, y).
    fn chroma_part(&self, x: u32, y: u32) -> usize {
        if !self.intra_split {
            return 0;
        }
        let half = 1 << (self.log2_size - 1);
        usize::from(x - self.x >= half) + 2 * usize::from(y - self.y >= half)
    }
}

pub struct SliceDecoder<'a, 'p> {
    sps: &'a Sps,
    pps: &'a Pps,
    header: &'a SliceHeader,
    header_index: u32,
    picture: &'p mut Picture,
    carry: &'p mut Carry,
    cabac: Cabac<'a>,
    contexts: ContextSet,
    qp_bd_offset_y: i32,
    qp_bd_offset_c: i32,
    log2_qg_size: u32,
    log2_chroma_qg_size: u32,
    qp_y: i32,
    qp_y_pred: i32,
    last_qp_y: i32,
    qp_delta_coded: bool,
    qp_delta: i32,
    chroma_offset_coded: bool,
    chroma_offset: (i32, i32),
    coeffs: Box<[i32; MAX_COEFFS]>,
    luma_residual: Box<[i32; MAX_COEFFS]>,
}

impl<'a, 'p> SliceDecoder<'a, 'p> {
    pub fn new(
        sps: &'a Sps,
        pps: &'a Pps,
        header: &'a SliceHeader,
        header_index: u32,
        rbsp: &'a [u8],
        picture: &'p mut Picture,
        carry: &'p mut Carry,
    ) -> Self {
        let qp_bd_offset_y = 6 * (sps.bit_depth_luma as i32 - 8);
        let log2_qg_size = sps.log2_ctb - pps.cu_qp_delta_depth.unwrap_or(0);
        let chroma_depth = pps.chroma_qp_offset_list.as_ref().map_or(0, |(depth, _)| *depth);
        Self {
            sps,
            pps,
            header,
            header_index,
            picture,
            carry,
            cabac: Cabac::new(rbsp, header.data_offset),
            contexts: ContextSet::new(header.qp_y),
            qp_bd_offset_y,
            qp_bd_offset_c: 6 * (sps.bit_depth_chroma as i32 - 8),
            log2_qg_size,
            log2_chroma_qg_size: sps.log2_ctb - chroma_depth,
            qp_y: header.qp_y,
            qp_y_pred: header.qp_y,
            last_qp_y: header.qp_y,
            qp_delta_coded: false,
            qp_delta: 0,
            chroma_offset_coded: false,
            chroma_offset: (0, 0),
            coeffs: Box::new([0; MAX_COEFFS]),
            luma_residual: Box::new([0; MAX_COEFFS]),
        }
    }

    /// Decodes CTUs from the segment address until end_of_slice_segment_flag.
    pub fn decode(mut self) -> Result<(), DecodeError> {
        let width_in_ctbs = self.picture.width_in_ctbs;
        let total = self.picture.tiles.ts_to_rs.len();
        let mut ts = self.picture.tiles.rs_to_ts[self.header.segment_address as usize] as usize;
        let mut first = true;
        let wpp = self.pps.entropy_coding_sync;

        loop {
            let rs = self.picture.tiles.ts_to_rs[ts] as usize;
            if self.picture.ctb_slice_address[rs] != NOT_DECODED {
                return Err(DecodeError::corrupt("HEVC slices overlap"));
            }
            self.picture.ctb_slice_address[rs] = self.header.slice_address;
            self.picture.ctb_slice[rs] = self.header_index;
            let (ctb_x, ctb_y) = (rs as u32 % width_in_ctbs, rs as u32 / width_in_ctbs);
            let tiles = &self.picture.tiles;
            let tile_start =
                ts == 0 || tiles.tile_id[rs] != tiles.tile_id[tiles.ts_to_rs[ts - 1] as usize];
            let row_start = wpp && ctb_x == tiles.column_start(ctb_x);
            if first || tile_start || row_start {
                self.start_entropy_segment(ctb_x, ctb_y, first, tile_start, row_start);
            }
            if self.header.sao_luma || self.header.sao_chroma {
                self.parse_sao(ctb_x, ctb_y, rs);
            }
            let log2_ctb = self.sps.log2_ctb;
            self.coding_quadtree(ctb_x << log2_ctb, ctb_y << log2_ctb, log2_ctb, 0)?;
            let end_of_segment = self.cabac.terminate();
            if self.cabac.exhausted() {
                return Err(DecodeError::corrupt("HEVC slice data is truncated"));
            }

            let tiles = &self.picture.tiles;
            let second_in_row =
                ctb_x == 1 || (rs >= 2 && tiles.tile_id[rs] != tiles.tile_id[rs - 2]);
            if wpp && second_in_row {
                self.carry.wpp = Some(self.contexts.clone());
            }
            ts += 1;
            if end_of_segment {
                if self.pps.dependent_slices {
                    self.carry.dependent = Some((self.contexts.clone(), self.last_qp_y));
                }
                return Ok(());
            }
            if ts >= total {
                return Err(DecodeError::corrupt("HEVC slice data runs past the picture"));
            }
            let next = tiles.ts_to_rs[ts] as usize;
            let next_x = next as u32 % width_in_ctbs;
            let new_tile = tiles.tile_id[next] != tiles.tile_id[rs];
            let new_row = wpp && next_x == tiles.column_start(next_x);
            if (self.pps.tiles.is_some() && new_tile) || new_row {
                if !self.cabac.terminate() {
                    return Err(DecodeError::corrupt("HEVC substream does not end where expected"));
                }
                self.cabac.restart(self.cabac.position());
            }
            first = false;
        }
    }

    /// 9.3.1: context variables at the start of a segment, tile or WPP row.
    fn start_entropy_segment(
        &mut self,
        ctb_x: u32,
        ctb_y: u32,
        first: bool,
        tile_start: bool,
        row_start: bool,
    ) {
        let slice_qp = self.header.qp_y;
        let fresh = || ContextSet::new(slice_qp);
        self.last_qp_y = slice_qp;
        self.contexts = if tile_start {
            fresh()
        } else if row_start {
            let size = 1_i32 << self.sps.log2_ctb;
            let (x, y) = ((ctb_x as i32) * size, (ctb_y as i32) * size);
            let above_right = self.picture.available(x as u32, y as u32, x + size, y - size);
            match (&self.carry.wpp, above_right) {
                (Some(saved), true) => saved.clone(),
                _ => fresh(),
            }
        } else if first && self.header.dependent {
            let (saved, qp) = self.carry.dependent.clone().unwrap_or_else(|| (fresh(), slice_qp));
            self.last_qp_y = qp;
            saved
        } else {
            fresh()
        };
    }

    fn parse_sao(&mut self, ctb_x: u32, ctb_y: u32, rs: usize) {
        let width = self.picture.width_in_ctbs as usize;
        let slice_address = self.header.slice_address as usize;
        let tile_id = &self.picture.tiles.tile_id;
        let models = &mut self.contexts.models;
        let mut merged = None;
        if ctb_x > 0
            && rs > slice_address
            && tile_id[rs] == tile_id[rs - 1]
            && self.cabac.decode(&mut models[ctx::SAO_MERGE])
        {
            merged = Some(rs - 1);
        }
        if merged.is_none()
            && ctb_y > 0
            && rs - width >= slice_address
            && tile_id[rs] == tile_id[rs - width]
            && self.cabac.decode(&mut models[ctx::SAO_MERGE])
        {
            merged = Some(rs - width);
        }
        if let Some(source) = merged {
            self.picture.sao[rs] = self.picture.sao[source];
            return;
        }
        let components = if self.sps.chroma_format_idc == 0 { 1 } else { 3 };
        let mut params = [SaoComponent::default(); 3];
        for component in 0..components {
            let enabled =
                if component == 0 { self.header.sao_luma } else { self.header.sao_chroma };
            if !enabled {
                continue;
            }
            let (bit_depth, scale) = if component == 0 {
                (self.sps.bit_depth_luma, self.pps.log2_sao_offset_scale_luma)
            } else {
                (self.sps.bit_depth_chroma, self.pps.log2_sao_offset_scale_chroma)
            };
            let kind = if component == 2 {
                params[1].kind
            } else if !self.cabac.decode(&mut self.contexts.models[ctx::SAO_TYPE]) {
                0
            } else if self.cabac.bypass() {
                SAO_EDGE
            } else {
                SAO_BAND
            };
            if kind == 0 {
                continue;
            }
            let max = (1 << (bit_depth.min(10) - 5)) - 1;
            let mut offsets = [0_i16; 4];
            for offset in &mut offsets {
                let mut value = 0;
                while value < max && self.cabac.bypass() {
                    value += 1;
                }
                *offset = value;
            }
            let detail = if kind == SAO_BAND {
                for offset in &mut offsets {
                    if *offset != 0 && self.cabac.bypass() {
                        *offset = -*offset;
                    }
                }
                self.cabac.bypass_bits(5) as u8
            } else {
                offsets[2] = -offsets[2];
                offsets[3] = -offsets[3];
                if component == 2 { params[1].detail } else { self.cabac.bypass_bits(2) as u8 }
            };
            params[component] = SaoComponent { kind, detail, offsets: offsets.map(|o| o << scale) };
        }
        self.picture.sao[rs] = params;
    }

    fn coding_quadtree(
        &mut self,
        x0: u32,
        y0: u32,
        log2: u32,
        depth: u8,
    ) -> Result<(), DecodeError> {
        let size = 1 << log2;
        let (width, height) = (self.picture.width, self.picture.height);
        let split = if x0 + size <= width && y0 + size <= height && log2 > self.sps.log2_min_cb {
            let mut increment = 0;
            let (x, y) = (x0 as i32, y0 as i32);
            if self.picture.available(x0, y0, x - 1, y)
                && self.picture.ct_depth[self.picture.unit(x0 - 1, y0)] > depth
            {
                increment += 1;
            }
            if self.picture.available(x0, y0, x, y - 1)
                && self.picture.ct_depth[self.picture.unit(x0, y0 - 1)] > depth
            {
                increment += 1;
            }
            self.cabac.decode(&mut self.contexts.models[ctx::SPLIT_CU + increment])
        } else {
            log2 > self.sps.log2_min_cb
        };
        if log2 >= self.log2_qg_size {
            self.start_quantization_group(x0, y0);
        }
        if self.header.cu_chroma_qp_offset && log2 >= self.log2_chroma_qg_size {
            self.chroma_offset_coded = false;
        }
        if split {
            let half = size / 2;
            for (dx, dy) in [(0, 0), (half, 0), (0, half), (half, half)] {
                if x0 + dx < width && y0 + dy < height {
                    self.coding_quadtree(x0 + dx, y0 + dy, log2 - 1, depth + 1)?;
                }
            }
            return Ok(());
        }
        self.coding_unit(x0, y0, log2, depth)
    }

    /// 8.6.1: qPY_PRED for the quantization group starting at (xq, yq).
    fn start_quantization_group(&mut self, xq: u32, yq: u32) {
        self.qp_delta_coded = false;
        self.qp_delta = 0;
        let mask = (1 << self.sps.log2_ctb) - 1;
        let previous = self.last_qp_y;
        let qp_at = |x, y| i32::from(self.picture.qp_y[self.picture.unit(x, y)]);
        let left = if xq & mask != 0 { qp_at(xq - 1, yq) } else { previous };
        let above = if yq & mask != 0 { qp_at(xq, yq - 1) } else { previous };
        self.qp_y_pred = (left + above + 1) >> 1;
        self.qp_y = self.qp_y_pred;
    }

    fn coding_unit(&mut self, x0: u32, y0: u32, log2: u32, depth: u8) -> Result<(), DecodeError> {
        let size = 1 << log2;
        let width4 = self.picture.width4;
        Picture::fill(&mut self.picture.ct_depth, width4, x0, y0, size, depth);
        let bypass = self.pps.transquant_bypass
            && self.cabac.decode(&mut self.contexts.models[ctx::TRANSQUANT_BYPASS]);
        let intra_split = log2 == self.sps.log2_min_cb
            && !self.cabac.decode(&mut self.contexts.models[ctx::PART_MODE]);
        let pcm = !intra_split
            && self.sps.pcm.is_some_and(|p| (p.log2_min_size..=p.log2_max_size).contains(&log2))
            && self.cabac.terminate();
        let mut unfiltered = bypass;

        if pcm {
            Picture::fill(&mut self.picture.intra_mode, width4, x0, y0, size, DC);
            self.decode_pcm(x0, y0, log2)?;
            unfiltered |= self.sps.pcm.is_some_and(|p| p.loop_filter_disabled);
            self.mark_edges(x0, y0, size);
        } else {
            let cu = self.parse_intra_modes(x0, y0, log2, bypass, intra_split);
            let max_depth = self.sps.max_transform_depth_intra + u32::from(intra_split);
            self.transform_tree(&cu, (x0, y0), (x0, y0), log2, 0, max_depth, 0, 0)?;
        }
        if unfiltered {
            Picture::fill_flags(&mut self.picture.flags, width4, x0, y0, size, flags::UNFILTERED);
        }
        Picture::fill(&mut self.picture.qp_y, width4, x0, y0, size, self.qp_y as i8);
        self.last_qp_y = self.qp_y;
        Ok(())
    }

    /// prev_intra_luma_pred_flag, mpm_idx, rem_intra_luma_pred_mode and the chroma modes.
    fn parse_intra_modes(
        &mut self,
        x0: u32,
        y0: u32,
        log2: u32,
        bypass: bool,
        intra_split: bool,
    ) -> CodingUnit {
        let parts = if intra_split { 4 } else { 1 };
        let part_size = if intra_split { 1 << (log2 - 1) } else { 1 << log2 };
        let mut from_list = [false; 4];
        for flag in from_list.iter_mut().take(parts) {
            *flag = self.cabac.decode(&mut self.contexts.models[ctx::PREV_INTRA_LUMA]);
        }
        let mut luma_modes = [DC; 4];
        for (part, &in_list) in from_list.iter().enumerate().take(parts) {
            let (x, y) = (x0 + (part as u32 % 2) * part_size, y0 + (part as u32 / 2) * part_size);
            let mut candidates = self.most_probable_modes(x, y);
            let mode = if in_list {
                let index = if !self.cabac.bypass() {
                    0
                } else if !self.cabac.bypass() {
                    1
                } else {
                    2
                };
                candidates[index]
            } else {
                let mut mode = self.cabac.bypass_bits(5) as u8;
                candidates.sort_unstable();
                for candidate in candidates {
                    if mode >= candidate {
                        mode += 1;
                    }
                }
                mode
            };
            let width4 = self.picture.width4;
            Picture::fill(&mut self.picture.intra_mode, width4, x, y, part_size, mode);
            luma_modes[part] = mode;
        }
        let chroma_format = self.sps.chroma_format_idc;
        let chroma_parts = match chroma_format {
            0 => 0,
            3 => parts,
            _ => 1,
        };
        let mut chroma_modes = [DC; 4];
        let mut chroma_dm = [false; 4];
        for part in 0..chroma_parts {
            let syntax = if self.cabac.decode(&mut self.contexts.models[ctx::INTRA_CHROMA]) {
                self.cabac.bypass_bits(2) as u8
            } else {
                4
            };
            let luma = luma_modes[part];
            let mut mode = match syntax {
                0 => intra::PLANAR,
                1 => intra::VERTICAL,
                2 => intra::HORIZONTAL,
                3 => DC,
                _ => luma,
            };
            if syntax < 4 && mode == luma {
                mode = 34;
            }
            if chroma_format == 2 {
                mode = MODE_422[usize::from(mode)];
            }
            chroma_modes[part] = mode;
            chroma_dm[part] = syntax == 4;
        }
        CodingUnit { x: x0, y: y0, log2_size: log2, bypass, intra_split, chroma_modes, chroma_dm }
    }

    /// 8.4.2: candModeList for the prediction block at (x, y).
    fn most_probable_modes(&self, x: u32, y: u32) -> [u8; 3] {
        let picture = &*self.picture;
        let (xi, yi) = (x as i32, y as i32);
        let left = if picture.available(x, y, xi - 1, yi) {
            picture.intra_mode[picture.unit(x - 1, y)]
        } else {
            DC
        };
        let ctb_top = (y >> picture.log2_ctb) << picture.log2_ctb;
        let above = if y > ctb_top && picture.available(x, y, xi, yi - 1) {
            picture.intra_mode[picture.unit(x, y - 1)]
        } else {
            DC
        };
        if left == above {
            if left < 2 {
                [intra::PLANAR, DC, intra::VERTICAL]
            } else {
                [left, 2 + ((left + 29) % 32), 2 + ((left - 2 + 1) % 32)]
            }
        } else {
            let third = if left != intra::PLANAR && above != intra::PLANAR {
                intra::PLANAR
            } else if left != DC && above != DC {
                DC
            } else {
                intra::VERTICAL
            };
            [left, above, third]
        }
    }

    fn decode_pcm(&mut self, x0: u32, y0: u32, log2: u32) -> Result<(), DecodeError> {
        let Some(pcm) = self.sps.pcm else {
            return Err(DecodeError::corrupt("HEVC PCM without PCM parameters"));
        };
        let start = self.cabac.position();
        let mut reader = BitReader::new(&self.cabac.data()[start..]);
        let size = 1_usize << log2;
        let mut blocks = vec![(0, x0 as usize, y0 as usize, size, size, pcm.bit_depth_luma)];
        if self.sps.chroma_format_idc != 0 {
            let (sx, sy) = self.picture.chroma_shift;
            let (xc, yc) = (x0 as usize >> sx, y0 as usize >> sy);
            for component in 1..3 {
                blocks.push((component, xc, yc, size >> sx, size >> sy, pcm.bit_depth_chroma));
            }
        }
        for (component, x, y, width, height, depth) in blocks {
            let plane = &mut self.picture.planes[component];
            let shift = plane.bit_depth - depth;
            for row in 0..height {
                let start = (y + row) * plane.width + x;
                for sample in &mut plane.samples[start..start + width] {
                    *sample = (reader.bits(depth)? << shift) as u16;
                }
            }
        }
        let consumed = reader.byte_align();
        self.cabac.restart(start + consumed);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn transform_tree(
        &mut self,
        cu: &CodingUnit,
        (x0, y0): (u32, u32),
        (x_base, y_base): (u32, u32),
        log2: u32,
        depth: u32,
        max_depth: u32,
        blk_idx: u32,
        parent_cbf_chroma: u8,
    ) -> Result<(), DecodeError> {
        let sps = self.sps;
        let split = if log2 <= sps.log2_max_tb
            && log2 > sps.log2_min_tb
            && depth < max_depth
            && !(cu.intra_split && depth == 0)
        {
            self.cabac.decode(&mut self.contexts.models[ctx::SPLIT_TRANSFORM + 5 - log2 as usize])
        } else {
            log2 > sps.log2_max_tb || (cu.intra_split && depth == 0)
        };
        let chroma_format = sps.chroma_format_idc;
        // Bits 0/1: Cb (top, bottom for 4:2:2); bits 2/3: Cr.
        let mut cbf_chroma = 0_u8;
        if (log2 > 2 && chroma_format != 0) || chroma_format == 3 {
            for component in 0..2 {
                if depth == 0 || parent_cbf_chroma & (1 << (2 * component)) != 0 {
                    let model = &mut self.contexts.models[ctx::CBF_CHROMA + depth as usize];
                    let mut bits = u8::from(self.cabac.decode(model));
                    if chroma_format == 2 && (!split || log2 == 3) {
                        let model = &mut self.contexts.models[ctx::CBF_CHROMA + depth as usize];
                        bits |= u8::from(self.cabac.decode(model)) << 1;
                    }
                    cbf_chroma |= bits << (2 * component);
                }
            }
        }
        if split {
            let half = 1 << (log2 - 1);
            for (index, (dx, dy)) in
                [(0, 0), (half, 0), (0, half), (half, half)].into_iter().enumerate()
            {
                let origin = (x0 + dx, y0 + dy);
                self.transform_tree(
                    cu,
                    origin,
                    (x0, y0),
                    log2 - 1,
                    depth + 1,
                    max_depth,
                    index as u32,
                    cbf_chroma,
                )?;
            }
            return Ok(());
        }
        let model = &mut self.contexts.models[ctx::CBF_LUMA + usize::from(depth == 0)];
        let cbf_luma = self.cabac.decode(model);
        self.transform_unit(
            cu,
            (x0, y0),
            (x_base, y_base),
            log2,
            blk_idx,
            cbf_luma,
            cbf_chroma,
            parent_cbf_chroma,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn transform_unit(
        &mut self,
        cu: &CodingUnit,
        (x0, y0): (u32, u32),
        (x_base, y_base): (u32, u32),
        log2: u32,
        blk_idx: u32,
        cbf_luma: bool,
        cbf_chroma: u8,
        parent_cbf_chroma: u8,
    ) -> Result<(), DecodeError> {
        let chroma_format = self.sps.chroma_format_idc;
        let chroma_here = chroma_format != 0 && (log2 > 2 || chroma_format == 3);
        let chroma_deferred = chroma_format != 0 && !chroma_here;
        let chroma_flags = if chroma_deferred { parent_cbf_chroma } else { cbf_chroma };

        if cbf_luma || chroma_flags != 0 {
            if self.pps.cu_qp_delta_depth.is_some() && !self.qp_delta_coded {
                self.parse_qp_delta()?;
            }
            if self.header.cu_chroma_qp_offset
                && chroma_flags != 0
                && !cu.bypass
                && !self.chroma_offset_coded
            {
                self.parse_chroma_qp_offset();
            }
        }
        let mode = self.picture.intra_mode[self.picture.unit(x0, y0)];
        self.predict(cu, 0, x0, y0, log2, mode);
        if cbf_luma {
            self.residual_block(cu, 0, x0, y0, log2, mode, 0)?;
        }
        self.mark_edges(x0, y0, 1 << log2);

        if chroma_here {
            let log2_c = if chroma_format == 3 { log2 } else { log2 - 1 };
            self.chroma_blocks(cu, (x0, y0), log2_c, cbf_chroma, cbf_luma)?;
        } else if chroma_deferred && blk_idx == 3 {
            self.chroma_blocks(cu, (x_base, y_base), 2, parent_cbf_chroma, false)?;
        }
        Ok(())
    }

    /// Cb then Cr blocks of a transform unit whose luma origin is (x, y).
    fn chroma_blocks(
        &mut self,
        cu: &CodingUnit,
        (x, y): (u32, u32),
        log2_c: u32,
        cbf: u8,
        cbf_luma: bool,
    ) -> Result<(), DecodeError> {
        let (sx, sy) = self.picture.chroma_shift;
        let part = cu.chroma_part(x, y);
        let mode = cu.chroma_modes[part];
        let blocks = if self.sps.chroma_format_idc == 2 { 2 } else { 1 };
        for component in 1..3 {
            let res_scale = if self.pps.cross_component_prediction && cbf_luma && cu.chroma_dm[part]
            {
                self.parse_cross_component(component - 1)
            } else {
                0
            };
            for block in 0..blocks {
                let (xc, yc) = (x >> sx, (y >> sy) + (block << log2_c));
                self.predict(cu, component, xc, yc, log2_c, mode);
                if cbf & (1 << (2 * (component - 1) + block as usize)) != 0 {
                    self.residual_block(cu, component, xc, yc, log2_c, mode, res_scale)?;
                } else if res_scale != 0 {
                    // Cross-component prediction alone still adds a residual.
                    self.coeffs[..1 << (2 * log2_c)].fill(0);
                    self.add_residual(component, xc, yc, log2_c, res_scale);
                }
            }
        }
        Ok(())
    }

    fn parse_qp_delta(&mut self) -> Result<(), DecodeError> {
        let models = &mut self.contexts.models;
        let mut magnitude = 0;
        while magnitude < CU_QP_DELTA_PREFIX
            && self.cabac.decode(&mut models[ctx::CU_QP_DELTA + usize::from(magnitude > 0)])
        {
            magnitude += 1;
        }
        if magnitude == CU_QP_DELTA_PREFIX {
            magnitude += self.exp_golomb_bypass()?;
        }
        let mut delta = magnitude as i32;
        if delta > 0 && self.cabac.bypass() {
            delta = -delta;
        }
        let limit = 26 + self.qp_bd_offset_y / 2;
        if !(-limit..=limit - 1).contains(&delta) {
            return Err(DecodeError::corrupt("HEVC CU QP delta is out of range"));
        }
        self.qp_delta_coded = true;
        self.qp_delta = delta;
        let range = 52 + self.qp_bd_offset_y;
        self.qp_y =
            (self.qp_y_pred + delta + 52 + 2 * self.qp_bd_offset_y) % range - self.qp_bd_offset_y;
        Ok(())
    }

    /// k-th order Exp-Golomb with k = 0, bypass coded.
    fn exp_golomb_bypass(&mut self) -> Result<u32, DecodeError> {
        let mut prefix = 0;
        while self.cabac.bypass() {
            prefix += 1;
            if prefix > MAX_GOLOMB_PREFIX {
                return Err(DecodeError::corrupt("HEVC Exp-Golomb bypass code is too long"));
            }
        }
        Ok((1 << prefix) - 1 + self.cabac.bypass_bits(prefix))
    }

    fn parse_chroma_qp_offset(&mut self) {
        let Some((_, list)) = &self.pps.chroma_qp_offset_list else {
            return;
        };
        self.chroma_offset_coded = true;
        if !self.cabac.decode(&mut self.contexts.models[ctx::CHROMA_QP_OFFSET_FLAG]) {
            self.chroma_offset = (0, 0);
            return;
        }
        let mut index = 0;
        while index + 1 < list.len()
            && self.cabac.decode(&mut self.contexts.models[ctx::CHROMA_QP_OFFSET_IDX])
        {
            index += 1;
        }
        self.chroma_offset = list[index];
    }

    /// cross_comp_pred(): ResScaleVal for one chroma component.
    fn parse_cross_component(&mut self, chroma: usize) -> i32 {
        let mut magnitude = 0;
        while magnitude < 4
            && self
                .cabac
                .decode(&mut self.contexts.models[ctx::LOG2_RES_SCALE + 4 * chroma + magnitude])
        {
            magnitude += 1;
        }
        if magnitude == 0 {
            return 0;
        }
        let negative = self.cabac.decode(&mut self.contexts.models[ctx::RES_SCALE_SIGN + chroma]);
        let value = 1 << (magnitude - 1);
        if negative { -value } else { value }
    }

    /// Predicts one block of `component` at component coordinates (x, y).
    fn predict(&mut self, cu: &CodingUnit, component: usize, x: u32, y: u32, log2: u32, mode: u8) {
        let (sx, sy) = if component == 0 { (0, 0) } else { self.picture.chroma_shift };
        let size = 1_i32 << log2;
        let (luma_x, luma_y) = (x << sx, y << sy);
        let mut available = [false; REFERENCE_LINE];
        let mut cached: Option<(i32, i32, bool)> = None;
        for (i, flag) in available.iter_mut().enumerate().take((4 * size + 1) as usize) {
            let i = i as i32;
            let (dx, dy) = match i.cmp(&(2 * size)) {
                std::cmp::Ordering::Less => (-1, 2 * size - 1 - i),
                std::cmp::Ordering::Equal => (-1, -1),
                std::cmp::Ordering::Greater => (i - 2 * size - 1, -1),
            };
            let (nx, ny) = ((x as i32 + dx) << sx, (y as i32 + dy) << sy);
            // Neighbours in the same 4x4 luma unit share availability.
            let unit = (nx >> 2, ny >> 2);
            *flag = match cached {
                Some((ux, uy, value)) if (ux, uy) == unit && nx >= 0 && ny >= 0 => value,
                _ => {
                    let value = self.picture.available(luma_x, luma_y, nx, ny);
                    cached = Some((unit.0, unit.1, value));
                    value
                }
            };
        }
        let range = &self.sps.range;
        let luma_like = component == 0 || self.sps.chroma_format_idc == 3;
        let options = IntraOptions {
            smoothing: luma_like && !range.intra_smoothing_disabled,
            strong_smoothing: component == 0 && self.sps.strong_intra_smoothing,
            edge_filters: component == 0,
            no_boundary_filter: range.implicit_rdpcm && cu.bypass,
        };
        let plane = &mut self.picture.planes[component];
        intra::predict(plane, x as usize, y as usize, log2, mode, &available, options);
    }

    /// Parses residual_coding() for one block and adds it to the prediction.
    #[allow(clippy::too_many_arguments)]
    fn residual_block(
        &mut self,
        cu: &CodingUnit,
        component: usize,
        x: u32,
        y: u32,
        log2: u32,
        mode: u8,
        res_scale: i32,
    ) -> Result<(), DecodeError> {
        let range = self.sps.range;
        let luma_like = component == 0 || self.sps.chroma_format_idc == 3;
        let rdpcm_mode = mode == intra::HORIZONTAL || mode == intra::VERTICAL;
        let params = ResidualParams {
            log2_size: log2,
            component,
            scan: residual::scan_index(log2, mode, luma_like),
            bypass: cu.bypass,
            transform_skip_allowed: self.pps.transform_skip
                && !cu.bypass
                && log2 <= self.pps.log2_max_transform_skip_size,
            sign_hiding: self.pps.sign_data_hiding,
            rdpcm_mode,
            implicit_rdpcm: range.implicit_rdpcm,
            transform_skip_context: range.transform_skip_context,
            persistent_rice: range.persistent_rice_adaptation,
        };
        let coefficients =
            residual::decode(&mut self.cabac, &mut self.contexts, &params, &mut self.coeffs[..])?;
        let skip = coefficients.transform_skip || cu.bypass;
        let (qp, bit_depth) = if component == 0 {
            (self.qp_y + self.qp_bd_offset_y, self.sps.bit_depth_luma)
        } else {
            (self.chroma_qp(component), self.sps.bit_depth_chroma)
        };
        let block = BlockParams {
            log2_size: log2,
            bit_depth,
            qp,
            transform_skip: coefficients.transform_skip,
            bypass: cu.bypass,
            dst: component == 0 && log2 == 2,
            rotate: range.transform_skip_rotation && log2 == 2 && skip,
            rdpcm: (range.implicit_rdpcm && skip && rdpcm_mode)
                .then_some(mode == intra::HORIZONTAL),
            extent: coefficients.extent,
        };
        let scaling = self.sps.scaling.as_ref().map(|sps_lists| {
            self.pps.scaling.as_ref().unwrap_or(sps_lists).matrix(log2, component)
        });
        transform::residual(&mut self.coeffs[..], &block, scaling);
        if component == 0 && self.pps.cross_component_prediction {
            let size = 1 << (2 * log2);
            self.luma_residual[..size].copy_from_slice(&self.coeffs[..size]);
        }
        self.add_residual(component, x, y, log2, res_scale);
        Ok(())
    }

    /// Adds `coeffs` (now residuals) to the predicted block, with cross-component prediction.
    fn add_residual(&mut self, component: usize, x: u32, y: u32, log2: u32, res_scale: i32) {
        let size = 1_usize << log2;
        if res_scale != 0 {
            let (luma_depth, chroma_depth) = (self.sps.bit_depth_luma, self.sps.bit_depth_chroma);
            for (value, &luma) in self.coeffs[..size * size].iter_mut().zip(&self.luma_residual[..])
            {
                *value += (res_scale * ((luma << chroma_depth) >> luma_depth)) >> 3;
            }
        }
        let plane = &mut self.picture.planes[component];
        let max = (1_i32 << plane.bit_depth) - 1;
        let stride = plane.width;
        for (row, residuals) in self.coeffs[..size * size].chunks_exact(size).enumerate() {
            let start = (y as usize + row) * stride + x as usize;
            for (sample, &residual) in plane.samples[start..start + size].iter_mut().zip(residuals)
            {
                *sample = (i32::from(*sample) + residual).clamp(0, max) as u16;
            }
        }
    }

    /// Qp'Cb or Qp'Cr for the current CU (8.6.1).
    fn chroma_qp(&self, component: usize) -> i32 {
        let offset = if component == 1 {
            self.pps.cb_qp_offset + self.header.cb_qp_offset + self.chroma_offset.0
        } else {
            self.pps.cr_qp_offset + self.header.cr_qp_offset + self.chroma_offset.1
        };
        let qpi = (self.qp_y + offset).clamp(-self.qp_bd_offset_c, 57);
        chroma_qp_from_index(qpi, self.sps.chroma_format_idc) + self.qp_bd_offset_c
    }

    /// Records the left and top edges of a block on the 8x8 deblocking grid.
    fn mark_edges(&mut self, x0: u32, y0: u32, size: u32) {
        if self.header.deblocking_disabled {
            return;
        }
        let (x, y) = (x0 as i32, y0 as i32);
        if x0 % 8 == 0 && self.edge_allowed(x0, y0, x - 1, y) {
            for row in (y0..(y0 + size).min(self.picture.height)).step_by(4) {
                let unit = self.picture.unit(x0, row);
                self.picture.flags[unit] |= flags::VERTICAL_EDGE;
            }
        }
        if y0 % 8 == 0 && self.edge_allowed(x0, y0, x, y - 1) {
            let start = self.picture.unit(x0, y0);
            let units = (size.min(self.picture.width - x0) as usize).div_ceil(4);
            for flag in &mut self.picture.flags[start..start + units] {
                *flag |= flags::HORIZONTAL_EDGE;
            }
        }
    }

    /// filterEdgeFlag (8.7.2.3) for the edge between (x, y) and its neighbour (nx, ny).
    fn edge_allowed(&self, x: u32, y: u32, nx: i32, ny: i32) -> bool {
        if nx < 0 || ny < 0 {
            return false;
        }
        let picture = &*self.picture;
        let (current, neighbour) = (picture.ctb_at(x, y), picture.ctb_at(nx as u32, ny as u32));
        if current == neighbour {
            return true;
        }
        if picture.tiles.tile_id[current] != picture.tiles.tile_id[neighbour]
            && !self.pps.loop_filter_across_tiles
        {
            return false;
        }
        picture.ctb_slice_address[current] == picture.ctb_slice_address[neighbour]
            || self.header.loop_filter_across_slices
    }
}
