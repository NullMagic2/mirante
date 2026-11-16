//! Module: mirante_format_heic::hevc::params
//! Purpose: Sequence and picture parameter sets (H.265 7.3.2.2, 7.3.2.3).
//! Created: 2026-11-16
//! Architecture: Parsed once per NAL unit and validated against the limits the
//! rest of the decoder relies on; VPS content and VUI/HRD are read only to skip them.

use super::bitstream::BitReader;
use super::scaling::ScalingFactors;
use mirante_core::DecodeError;

pub const MAX_SPS_ID: u32 = 15;
pub const MAX_PPS_ID: u32 = 63;
const MAX_SUB_LAYERS: u32 = 7;
const MAX_SHORT_TERM_SETS: u32 = 64;
const MAX_LONG_TERM_SPS: u32 = 32;
/// A reference picture set can hold at most 16 pictures (sps_max_dec_pic_buffering).
const MAX_DELTA_POCS: usize = 16;
const MAX_BIT_DEPTH: u32 = 12;
const MAX_CTB_LOG2: u32 = 6;
const MIN_CTB_LOG2: u32 = 4;
const MAX_TB_LOG2: u32 = 5;
const MAX_CHROMA_QP_OFFSETS: u32 = 6;

/// sps_range_extension() flags that change intra decoding.
#[derive(Clone, Copy, Debug, Default)]
pub struct RangeExtension {
    pub transform_skip_rotation: bool,
    pub transform_skip_context: bool,
    pub implicit_rdpcm: bool,
    pub intra_smoothing_disabled: bool,
    pub persistent_rice_adaptation: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Pcm {
    pub bit_depth_luma: u32,
    pub bit_depth_chroma: u32,
    pub log2_min_size: u32,
    pub log2_max_size: u32,
    pub loop_filter_disabled: bool,
}

/// A short-term reference picture set, kept only so later sets can predict from it.
#[derive(Clone, Debug, Default)]
pub struct ShortTermSet {
    negative: Vec<i32>,
    positive: Vec<i32>,
}

impl ShortTermSet {
    fn count(&self) -> usize {
        self.negative.len() + self.positive.len()
    }

    /// st_ref_pic_set(index), where `sets` holds the sets parsed before it.
    pub fn parse(
        reader: &mut BitReader<'_>,
        sets: &[ShortTermSet],
        in_slice_header: bool,
    ) -> Result<Self, DecodeError> {
        let index = sets.len();
        if index > 0 && reader.flag()? {
            let delta_index = if in_slice_header {
                reader.ue_max(index as u32 - 1, "short-term set index delta")? + 1
            } else {
                1
            };
            let reference = &sets[index - delta_index as usize];
            let sign = reader.flag()?;
            let magnitude = reader.ue_max(1 << 15, "short-term set delta")? as i32 + 1;
            let delta_rps = if sign { -magnitude } else { magnitude };
            let (negative, positive) = (&reference.negative, &reference.positive);
            let total = negative.len() + positive.len();
            let mut use_delta = [false; MAX_DELTA_POCS + 1];
            for flag in &mut use_delta[..=total] {
                *flag = reader.flag()? || reader.flag()?;
            }
            // The order follows 7.4.8 so that later sets predict from the same indices.
            let used_negative = use_delta[..negative.len()].iter();
            let used_positive = use_delta[negative.len()..total].iter();
            let negatives = negative.iter().zip(used_negative.clone());
            let positives = positive.iter().zip(used_positive.clone());
            let own = (&0, &use_delta[total]);
            let mut set = Self::default();
            for (&delta, &used) in positives.clone().rev().chain([own]).chain(negatives.clone()) {
                if used && delta + delta_rps < 0 {
                    set.negative.push(delta + delta_rps);
                }
            }
            for (&delta, &used) in negatives.rev().chain([own]).chain(positives) {
                if used && delta + delta_rps > 0 {
                    set.positive.push(delta + delta_rps);
                }
            }
            return set.checked();
        }
        let negative = reader.ue_max(MAX_DELTA_POCS as u32, "negative reference pictures")?;
        let positive = reader.ue_max(MAX_DELTA_POCS as u32, "positive reference pictures")?;
        let mut set = Self::default();
        let mut poc = 0;
        for _ in 0..negative {
            poc -= reader.ue_max(1 << 15, "reference POC delta")? as i32 + 1;
            reader.flag()?;
            set.negative.push(poc);
        }
        poc = 0;
        for _ in 0..positive {
            poc += reader.ue_max(1 << 15, "reference POC delta")? as i32 + 1;
            reader.flag()?;
            set.positive.push(poc);
        }
        set.checked()
    }

    fn checked(self) -> Result<Self, DecodeError> {
        if self.count() > MAX_DELTA_POCS {
            return Err(DecodeError::corrupt("HEVC reference picture set is too large"));
        }
        Ok(self)
    }
}

#[derive(Clone, Debug)]
pub struct Sps {
    pub id: u32,
    /// ChromaArrayType; separate colour planes are rejected.
    pub chroma_format_idc: u32,
    pub width: u32,
    pub height: u32,
    /// Conformance window in luma samples: left, right, top, bottom.
    pub conformance: [u32; 4],
    pub bit_depth_luma: u32,
    pub bit_depth_chroma: u32,
    pub log2_max_poc_lsb: u32,
    pub log2_min_cb: u32,
    pub log2_ctb: u32,
    pub log2_min_tb: u32,
    pub log2_max_tb: u32,
    pub max_transform_depth_intra: u32,
    /// None when scaling lists are disabled.
    pub scaling: Option<ScalingFactors>,
    pub sao: bool,
    pub pcm: Option<Pcm>,
    pub short_term_sets: Vec<ShortTermSet>,
    pub long_term_pictures: Option<u32>,
    pub temporal_mvp: bool,
    pub strong_intra_smoothing: bool,
    pub range: RangeExtension,
}

impl Sps {
    pub fn parse(rbsp: &[u8]) -> Result<Self, DecodeError> {
        let mut reader = BitReader::new(rbsp);
        reader.skip(4)?;
        let max_sub_layers_minus1 = reader.bits(3)?;
        if max_sub_layers_minus1 >= MAX_SUB_LAYERS {
            return Err(DecodeError::corrupt("HEVC SPS sub-layer count"));
        }
        reader.skip(1)?;
        skip_profile_tier_level(&mut reader, max_sub_layers_minus1)?;
        let id = reader.ue_max(MAX_SPS_ID, "SPS id")?;
        let chroma_format_idc = reader.ue_max(3, "chroma format")?;
        if chroma_format_idc == 3 && reader.flag()? {
            return Err(DecodeError::unsupported("HEVC separate colour planes"));
        }
        let width = reader.ue()?;
        let height = reader.ue()?;
        DecodeError::check_size(width, height)?;
        let mut conformance = [0; 4];
        if reader.flag()? {
            let (sub_width, sub_height) = chroma_subsampling(chroma_format_idc);
            for (i, offset) in conformance.iter_mut().enumerate() {
                let unit = if i < 2 { sub_width } else { sub_height };
                *offset = reader.ue_max(u32::from(u16::MAX), "conformance window")? * unit;
            }
        }
        if conformance[0] + conformance[1] >= width || conformance[2] + conformance[3] >= height {
            return Err(DecodeError::corrupt("HEVC conformance window is empty"));
        }
        let bit_depth_luma = reader.ue()?.saturating_add(8);
        let bit_depth_chroma = reader.ue()?.saturating_add(8);
        if bit_depth_luma > MAX_BIT_DEPTH || bit_depth_chroma > MAX_BIT_DEPTH {
            return Err(DecodeError::unsupported(format!("HEVC bit depth {bit_depth_luma}")));
        }
        let log2_max_poc_lsb = reader.ue_max(12, "POC LSB length")? + 4;
        let all_sub_layers = reader.flag()?;
        let first = if all_sub_layers { 0 } else { max_sub_layers_minus1 };
        for _ in first..=max_sub_layers_minus1 {
            for _ in 0..3 {
                reader.ue()?;
            }
        }
        let log2_min_cb = reader.ue_max(MAX_CTB_LOG2 - 3, "minimum coding block size")? + 3;
        let log2_ctb = log2_min_cb + reader.ue_max(MAX_CTB_LOG2, "coding tree block size")?;
        let log2_min_tb = reader.ue_max(MAX_TB_LOG2 - 2, "minimum transform size")? + 2;
        let log2_max_tb = log2_min_tb + reader.ue_max(MAX_TB_LOG2, "maximum transform size")?;
        if !(MIN_CTB_LOG2..=MAX_CTB_LOG2).contains(&log2_ctb)
            || log2_min_tb >= log2_min_cb
            || log2_max_tb > log2_ctb.min(MAX_TB_LOG2)
        {
            return Err(DecodeError::unsupported("HEVC block size configuration"));
        }
        if width % (1 << log2_min_cb) != 0 || height % (1 << log2_min_cb) != 0 {
            return Err(DecodeError::corrupt("HEVC picture size is not a multiple of the CU size"));
        }
        reader.ue()?;
        let max_transform_depth_intra = reader.ue_max(log2_ctb - log2_min_tb, "transform depth")?;
        let scaling = match reader.flag()? {
            false => None,
            true if reader.flag()? => Some(ScalingFactors::parse(&mut reader)?),
            true => Some(ScalingFactors::default_lists()),
        };
        reader.flag()?;
        let sao = reader.flag()?;
        let pcm = if reader.flag()? {
            let bit_depth_luma = reader.bits(4)? + 1;
            let bit_depth_chroma = reader.bits(4)? + 1;
            let log2_min_size = reader.ue_max(2, "PCM size")? + 3;
            let log2_max_size = log2_min_size + reader.ue_max(2, "PCM size range")?;
            if log2_max_size > log2_ctb.min(5) {
                return Err(DecodeError::corrupt("HEVC PCM parameters"));
            }
            Some(Pcm {
                bit_depth_luma,
                bit_depth_chroma,
                log2_min_size,
                log2_max_size,
                loop_filter_disabled: reader.flag()?,
            })
        } else {
            None
        };
        let set_count = reader.ue_max(MAX_SHORT_TERM_SETS, "short-term set count")?;
        let mut short_term_sets = Vec::with_capacity(set_count as usize);
        for _ in 0..set_count {
            let set = ShortTermSet::parse(&mut reader, &short_term_sets, false)?;
            short_term_sets.push(set);
        }
        let long_term_pictures = if reader.flag()? {
            let count = reader.ue_max(MAX_LONG_TERM_SPS, "long-term picture count")?;
            for _ in 0..count {
                reader.skip(log2_max_poc_lsb as usize + 1)?;
            }
            Some(count)
        } else {
            None
        };
        let temporal_mvp = reader.flag()?;
        let strong_intra_smoothing = reader.flag()?;
        if reader.flag()? {
            skip_vui(&mut reader, max_sub_layers_minus1)?;
        }
        let range = match parse_extension_flags(&mut reader)? {
            true => parse_range_extension(&mut reader)?,
            false => RangeExtension::default(),
        };
        let sps = Self {
            id,
            chroma_format_idc,
            width,
            height,
            conformance,
            bit_depth_luma,
            bit_depth_chroma,
            log2_max_poc_lsb,
            log2_min_cb,
            log2_ctb,
            log2_min_tb,
            log2_max_tb,
            max_transform_depth_intra,
            scaling,
            sao,
            pcm,
            short_term_sets,
            long_term_pictures,
            temporal_mvp,
            strong_intra_smoothing,
            range,
        };
        if let Some(pcm) = sps.pcm {
            if pcm.bit_depth_luma > sps.bit_depth_luma
                || pcm.bit_depth_chroma > sps.bit_depth_chroma
            {
                return Err(DecodeError::corrupt("HEVC PCM bit depth exceeds the picture's"));
            }
        }
        Ok(sps)
    }

    /// (SubWidthC, SubHeightC) as shifts.
    pub fn chroma_shifts(&self) -> (u32, u32) {
        match self.chroma_format_idc {
            1 => (1, 1),
            2 => (1, 0),
            _ => (0, 0),
        }
    }

    pub fn width_in_ctbs(&self) -> u32 {
        self.width.div_ceil(1 << self.log2_ctb)
    }

    pub fn height_in_ctbs(&self) -> u32 {
        self.height.div_ceil(1 << self.log2_ctb)
    }
}

fn chroma_subsampling(chroma_format_idc: u32) -> (u32, u32) {
    match chroma_format_idc {
        1 => (2, 2),
        2 => (2, 1),
        _ => (1, 1),
    }
}

/// The SPS/PPS extension flags: whether a range extension follows. Screen content
/// coding changes the CU syntax, so it is refused; multilayer and 3D data is ignored.
fn parse_extension_flags(reader: &mut BitReader<'_>) -> Result<bool, DecodeError> {
    if !reader.flag()? {
        return Ok(false);
    }
    let range = reader.flag()?;
    reader.skip(2)?;
    if reader.flag()? {
        return Err(DecodeError::unsupported("HEVC screen content coding extensions"));
    }
    reader.skip(4)?;
    Ok(range)
}

fn parse_range_extension(reader: &mut BitReader<'_>) -> Result<RangeExtension, DecodeError> {
    let transform_skip_rotation = reader.flag()?;
    let transform_skip_context = reader.flag()?;
    let implicit_rdpcm = reader.flag()?;
    reader.flag()?;
    if reader.flag()? {
        return Err(DecodeError::unsupported("HEVC extended precision processing"));
    }
    let intra_smoothing_disabled = reader.flag()?;
    reader.flag()?;
    let persistent_rice_adaptation = reader.flag()?;
    if reader.flag()? {
        return Err(DecodeError::unsupported("HEVC CABAC bypass alignment"));
    }
    Ok(RangeExtension {
        transform_skip_rotation,
        transform_skip_context,
        implicit_rdpcm,
        intra_smoothing_disabled,
        persistent_rice_adaptation,
    })
}

/// profile_tier_level(1, maxNumSubLayersMinus1); nothing in it affects decoding.
fn skip_profile_tier_level(reader: &mut BitReader<'_>, sub_layers: u32) -> Result<(), DecodeError> {
    reader.skip(96)?;
    let mut present = [(false, false); 8];
    for flags in present.iter_mut().take(sub_layers as usize) {
        *flags = (reader.flag()?, reader.flag()?);
    }
    if sub_layers > 0 {
        reader.skip(2 * (8 - sub_layers as usize))?;
    }
    for &(profile, level) in present.iter().take(sub_layers as usize) {
        reader.skip(if profile { 88 } else { 0 } + if level { 8 } else { 0 })?;
    }
    Ok(())
}

fn skip_vui(reader: &mut BitReader<'_>, sub_layers: u32) -> Result<(), DecodeError> {
    const EXTENDED_SAR: u32 = 255;
    if reader.flag()? && reader.bits(8)? == EXTENDED_SAR {
        reader.skip(32)?;
    }
    if reader.flag()? {
        reader.skip(1)?;
    }
    if reader.flag()? {
        reader.skip(4)?;
        if reader.flag()? {
            reader.skip(24)?;
        }
    }
    if reader.flag()? {
        reader.ue()?;
        reader.ue()?;
    }
    reader.skip(3)?;
    if reader.flag()? {
        for _ in 0..4 {
            reader.ue()?;
        }
    }
    if reader.flag()? {
        reader.skip(64)?;
        if reader.flag()? {
            reader.ue()?;
        }
        if reader.flag()? {
            skip_hrd(reader, sub_layers)?;
        }
    }
    if reader.flag()? {
        reader.skip(3)?;
        for _ in 0..5 {
            reader.ue()?;
        }
    }
    Ok(())
}

/// hrd_parameters(1, maxNumSubLayersMinus1)
fn skip_hrd(reader: &mut BitReader<'_>, sub_layers: u32) -> Result<(), DecodeError> {
    let nal = reader.flag()?;
    let vcl = reader.flag()?;
    let mut sub_picture = false;
    if nal || vcl {
        sub_picture = reader.flag()?;
        if sub_picture {
            reader.skip(19)?;
        }
        reader.skip(8)?;
        if sub_picture {
            reader.skip(4)?;
        }
        reader.skip(15)?;
    }
    for _ in 0..=sub_layers {
        let fixed_general = reader.flag()?;
        let fixed_within = fixed_general || reader.flag()?;
        let mut low_delay = false;
        if fixed_within {
            reader.ue()?;
        } else {
            low_delay = reader.flag()?;
        }
        let cpb_count = if low_delay { 1 } else { reader.ue_max(31, "HRD CPB count")? + 1 };
        let layers = usize::from(nal) + usize::from(vcl);
        for _ in 0..layers * cpb_count as usize {
            let values = if sub_picture { 4 } else { 2 };
            for _ in 0..values {
                reader.ue()?;
            }
            reader.skip(1)?;
        }
    }
    Ok(())
}

/// Tile column or row boundaries in CTBs, from 0 to the picture size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TileSpacing {
    Uniform { columns: u32, rows: u32 },
    Explicit { column_widths: Vec<u32>, row_heights: Vec<u32> },
}

#[derive(Clone, Debug)]
pub struct Deblocking {
    pub override_enabled: bool,
    pub disabled: bool,
    pub beta_offset: i32,
    pub tc_offset: i32,
}

#[derive(Clone, Debug)]
pub struct Pps {
    pub id: u32,
    pub sps_id: u32,
    pub dependent_slices: bool,
    pub output_flag_present: bool,
    pub extra_slice_header_bits: u32,
    pub sign_data_hiding: bool,
    pub init_qp: i32,
    pub transform_skip: bool,
    /// diff_cu_qp_delta_depth when cu_qp_delta is enabled.
    pub cu_qp_delta_depth: Option<u32>,
    pub cb_qp_offset: i32,
    pub cr_qp_offset: i32,
    pub slice_chroma_qp_offsets: bool,
    pub transquant_bypass: bool,
    pub tiles: Option<TileSpacing>,
    pub entropy_coding_sync: bool,
    pub loop_filter_across_tiles: bool,
    pub loop_filter_across_slices: bool,
    pub deblocking: Deblocking,
    pub scaling: Option<ScalingFactors>,
    pub slice_header_extension: bool,
    pub log2_max_transform_skip_size: u32,
    pub cross_component_prediction: bool,
    /// diff_cu_chroma_qp_offset_depth and the (Cb, Cr) offset list.
    pub chroma_qp_offset_list: Option<(u32, Vec<(i32, i32)>)>,
    pub log2_sao_offset_scale_luma: u32,
    pub log2_sao_offset_scale_chroma: u32,
}

impl Pps {
    pub fn parse(rbsp: &[u8]) -> Result<Self, DecodeError> {
        let mut reader = BitReader::new(rbsp);
        let id = reader.ue_max(MAX_PPS_ID, "PPS id")?;
        let sps_id = reader.ue_max(MAX_SPS_ID, "PPS SPS id")?;
        let dependent_slices = reader.flag()?;
        let output_flag_present = reader.flag()?;
        let extra_slice_header_bits = reader.bits(3)?;
        let sign_data_hiding = reader.flag()?;
        reader.flag()?;
        reader.ue_max(14, "reference index count")?;
        reader.ue_max(14, "reference index count")?;
        let init_qp = 26 + reader.se_range(-(26 + 6 * 8), 25, "initial QP")?;
        reader.flag()?;
        let transform_skip = reader.flag()?;
        let cu_qp_delta_depth = match reader.flag()? {
            true => Some(reader.ue_max(MAX_CTB_LOG2 - 3, "QP delta depth")?),
            false => None,
        };
        let cb_qp_offset = reader.se_range(-12, 12, "Cb QP offset")?;
        let cr_qp_offset = reader.se_range(-12, 12, "Cr QP offset")?;
        let slice_chroma_qp_offsets = reader.flag()?;
        reader.skip(2)?;
        let transquant_bypass = reader.flag()?;
        let tiles_enabled = reader.flag()?;
        let entropy_coding_sync = reader.flag()?;
        let mut tiles = None;
        let mut loop_filter_across_tiles = true;
        if tiles_enabled {
            let columns = reader.ue_max(u32::from(u16::MAX), "tile columns")? + 1;
            let rows = reader.ue_max(u32::from(u16::MAX), "tile rows")? + 1;
            tiles = Some(if reader.flag()? {
                TileSpacing::Uniform { columns, rows }
            } else {
                let mut sizes = |count: u32| -> Result<Vec<u32>, DecodeError> {
                    (1..count)
                        .map(|_| Ok(reader.ue_max(u32::from(u16::MAX), "tile size")? + 1))
                        .collect()
                };
                let column_widths = sizes(columns)?;
                let row_heights = sizes(rows)?;
                TileSpacing::Explicit { column_widths, row_heights }
            });
            loop_filter_across_tiles = reader.flag()?;
        }
        let loop_filter_across_slices = reader.flag()?;
        let mut deblocking =
            Deblocking { override_enabled: false, disabled: false, beta_offset: 0, tc_offset: 0 };
        if reader.flag()? {
            deblocking.override_enabled = reader.flag()?;
            deblocking.disabled = reader.flag()?;
            if !deblocking.disabled {
                deblocking.beta_offset = 2 * reader.se_range(-6, 6, "deblocking beta offset")?;
                deblocking.tc_offset = 2 * reader.se_range(-6, 6, "deblocking tC offset")?;
            }
        }
        let scaling = match reader.flag()? {
            true => Some(ScalingFactors::parse(&mut reader)?),
            false => None,
        };
        reader.flag()?;
        reader.ue()?;
        let slice_header_extension = reader.flag()?;
        let mut pps = Self {
            id,
            sps_id,
            dependent_slices,
            output_flag_present,
            extra_slice_header_bits,
            sign_data_hiding,
            init_qp,
            transform_skip,
            cu_qp_delta_depth,
            cb_qp_offset,
            cr_qp_offset,
            slice_chroma_qp_offsets,
            transquant_bypass,
            tiles,
            entropy_coding_sync,
            loop_filter_across_tiles,
            loop_filter_across_slices,
            deblocking,
            scaling,
            slice_header_extension,
            log2_max_transform_skip_size: 2,
            cross_component_prediction: false,
            chroma_qp_offset_list: None,
            log2_sao_offset_scale_luma: 0,
            log2_sao_offset_scale_chroma: 0,
        };
        if parse_extension_flags(&mut reader)? {
            pps.parse_range_extension(&mut reader)?;
        }
        Ok(pps)
    }

    fn parse_range_extension(&mut self, reader: &mut BitReader<'_>) -> Result<(), DecodeError> {
        if self.transform_skip {
            self.log2_max_transform_skip_size = reader.ue_max(3, "transform skip size")? + 2;
        }
        self.cross_component_prediction = reader.flag()?;
        if reader.flag()? {
            let depth = reader.ue_max(MAX_CTB_LOG2 - 3, "chroma QP offset depth")?;
            let count = reader.ue_max(MAX_CHROMA_QP_OFFSETS - 1, "chroma QP offset count")? + 1;
            let mut offsets = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let cb = reader.se_range(-12, 12, "Cb QP offset")?;
                offsets.push((cb, reader.se_range(-12, 12, "Cr QP offset")?));
            }
            self.chroma_qp_offset_list = Some((depth, offsets));
        }
        self.log2_sao_offset_scale_luma = reader.ue_max(6, "SAO offset scale")?;
        self.log2_sao_offset_scale_chroma = reader.ue_max(6, "SAO offset scale")?;
        Ok(())
    }

    /// Checks the PPS against the SPS it refers to.
    pub fn validate(&self, sps: &Sps) -> Result<(), DecodeError> {
        let corrupt = |what: &str| Err(DecodeError::corrupt(format!("HEVC PPS {what}")));
        if self.init_qp < -6 * (sps.bit_depth_luma as i32 - 8) {
            return corrupt("initial QP");
        }
        if self.cu_qp_delta_depth.is_some_and(|d| d > sps.log2_ctb - sps.log2_min_cb) {
            return corrupt("QP delta depth");
        }
        if self
            .chroma_qp_offset_list
            .as_ref()
            .is_some_and(|(d, _)| *d > sps.log2_ctb - sps.log2_min_cb)
        {
            return corrupt("chroma QP offset depth");
        }
        if self.log2_sao_offset_scale_luma > sps.bit_depth_luma.saturating_sub(10)
            || self.log2_sao_offset_scale_chroma > sps.bit_depth_chroma.saturating_sub(10)
        {
            return corrupt("SAO offset scale");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hevc::bitstream::writer::{pack, ue};

    #[test]
    fn predicts_short_term_sets_from_earlier_ones() {
        // Set 0: two negative pictures (-1, -3); set 1 predicts with deltaRps = -1.
        let bits = pack(&[ue(2), ue(0), ue(0), (1, 1), ue(1), (0, 1), (1, 1), (1, 1), ue(0)]);
        let mut reader = BitReader::new(&bits);
        let first = ShortTermSet::parse(&mut reader, &[], false).unwrap();
        assert_eq!(first.negative, [-1, -3]);
        let bits = pack(&[(1, 1), (1, 1), ue(0), (1, 1), (0, 1), (0, 1), (1, 1)]);
        let mut reader = BitReader::new(&bits);
        let second = ShortTermSet::parse(&mut reader, &[first], false).unwrap();
        assert_eq!(second.negative, [-1, -2]);
    }
}
