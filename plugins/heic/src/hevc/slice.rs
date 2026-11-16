//! Module: mirante_format_heic::hevc::slice
//! Purpose: The slice segment header (H.265 7.3.6) of intra slices.
//! Created: 2026-11-16
//! Architecture: Dependent slice segments copy the fields of the preceding
//! independent segment; the CTU decoder starts at `data_offset` in the RBSP.

use super::bitstream::BitReader;
use super::params::{Pps, ShortTermSet, Sps};
use mirante_core::DecodeError;

const SLICE_TYPE_I: u32 = 2;
const FIRST_IRAP: u8 = 16;
const LAST_IRAP: u8 = 23;
const IDR_W_RADL: u8 = 19;
const IDR_N_LP: u8 = 20;
const MAX_HEADER_EXTENSION: u32 = 256;

#[derive(Clone, Debug)]
pub struct SliceHeader {
    pub first_in_picture: bool,
    pub pps_id: u32,
    pub dependent: bool,
    /// slice_segment_address, a raster-scan CTB address.
    pub segment_address: u32,
    /// SliceAddrRs: the address of the independent segment this one belongs to.
    pub slice_address: u32,
    pub sao_luma: bool,
    pub sao_chroma: bool,
    pub qp_y: i32,
    pub cb_qp_offset: i32,
    pub cr_qp_offset: i32,
    pub cu_chroma_qp_offset: bool,
    pub deblocking_disabled: bool,
    pub beta_offset: i32,
    pub tc_offset: i32,
    pub loop_filter_across_slices: bool,
    /// Byte offset of slice_segment_data() in the RBSP.
    pub data_offset: usize,
}

impl SliceHeader {
    /// Parses a header; `previous` is the last independent header of the picture.
    pub fn parse<'s>(
        rbsp: &[u8],
        nal_type: u8,
        pps_for: impl Fn(u32) -> Result<(&'s Pps, &'s Sps), DecodeError>,
        previous: Option<&SliceHeader>,
    ) -> Result<Self, DecodeError> {
        let mut reader = BitReader::new(rbsp);
        let first_in_picture = reader.flag()?;
        if (FIRST_IRAP..=LAST_IRAP).contains(&nal_type) {
            reader.skip(1)?;
        }
        let pps_id = reader.ue_max(super::params::MAX_PPS_ID, "slice PPS id")?;
        let (pps, sps) = pps_for(pps_id)?;
        let ctb_count = sps.width_in_ctbs() * sps.height_in_ctbs();
        let mut dependent = false;
        let mut segment_address = 0;
        if !first_in_picture {
            dependent = pps.dependent_slices && reader.flag()?;
            segment_address = reader.bits(ceil_log2(ctb_count))?;
            if segment_address >= ctb_count {
                return Err(DecodeError::corrupt("HEVC slice address is outside the picture"));
            }
        }
        let mut header = if dependent {
            let previous = previous
                .filter(|p| p.pps_id == pps_id)
                .ok_or_else(|| DecodeError::corrupt("HEVC dependent slice without its slice"))?;
            Self {
                first_in_picture,
                dependent,
                segment_address,
                data_offset: 0,
                ..previous.clone()
            }
        } else {
            parse_independent(&mut reader, nal_type, pps, sps, first_in_picture, segment_address)?
        };
        if pps.tiles.is_some() || pps.entropy_coding_sync {
            let offsets = reader.ue_max(ctb_count, "entry point count")?;
            if offsets > 0 {
                let length = reader.ue_max(31, "entry point offset length")? + 1;
                reader.skip(offsets as usize * length as usize)?;
            }
        }
        if pps.slice_header_extension {
            let length = reader.ue_max(MAX_HEADER_EXTENSION, "slice header extension")?;
            reader.skip(length as usize * 8)?;
        }
        if !reader.bit()? {
            return Err(DecodeError::corrupt("HEVC slice header alignment bit"));
        }
        header.data_offset = reader.byte_align();
        Ok(header)
    }
}

fn parse_independent(
    reader: &mut BitReader<'_>,
    nal_type: u8,
    pps: &Pps,
    sps: &Sps,
    first_in_picture: bool,
    segment_address: u32,
) -> Result<SliceHeader, DecodeError> {
    reader.skip(pps.extra_slice_header_bits as usize)?;
    if reader.ue()? != SLICE_TYPE_I {
        return Err(DecodeError::unsupported("HEVC inter-coded (P/B) slices"));
    }
    if pps.output_flag_present {
        reader.skip(1)?;
    }
    if nal_type != IDR_W_RADL && nal_type != IDR_N_LP {
        skip_reference_pictures(reader, sps)?;
    }
    let sao_luma = sps.sao && reader.flag()?;
    let sao_chroma = sps.sao && sps.chroma_format_idc != 0 && reader.flag()?;
    let qp_bd_offset = 6 * (sps.bit_depth_luma as i32 - 8);
    let qp_y = pps.init_qp + reader.se_range(-(26 + qp_bd_offset), 25 + 26, "slice QP delta")?;
    if !(-qp_bd_offset..=51).contains(&qp_y) {
        return Err(DecodeError::corrupt("HEVC slice QP is out of range"));
    }
    let (mut cb_qp_offset, mut cr_qp_offset) = (0, 0);
    if pps.slice_chroma_qp_offsets {
        cb_qp_offset = reader.se_range(-12, 12, "slice Cb QP offset")?;
        cr_qp_offset = reader.se_range(-12, 12, "slice Cr QP offset")?;
        if !(-12..=12).contains(&(pps.cb_qp_offset + cb_qp_offset))
            || !(-12..=12).contains(&(pps.cr_qp_offset + cr_qp_offset))
        {
            return Err(DecodeError::corrupt("HEVC chroma QP offsets are out of range"));
        }
    }
    let cu_chroma_qp_offset = pps.chroma_qp_offset_list.is_some() && reader.flag()?;
    let defaults = &pps.deblocking;
    let (mut deblocking_disabled, mut beta_offset, mut tc_offset) =
        (defaults.disabled, defaults.beta_offset, defaults.tc_offset);
    if defaults.override_enabled && reader.flag()? {
        deblocking_disabled = reader.flag()?;
        if !deblocking_disabled {
            beta_offset = 2 * reader.se_range(-6, 6, "slice beta offset")?;
            tc_offset = 2 * reader.se_range(-6, 6, "slice tC offset")?;
        }
    }
    let loop_filter_across_slices =
        if pps.loop_filter_across_slices && (sao_luma || sao_chroma || !deblocking_disabled) {
            reader.flag()?
        } else {
            pps.loop_filter_across_slices
        };
    Ok(SliceHeader {
        first_in_picture,
        pps_id: pps.id,
        dependent: false,
        segment_address,
        slice_address: segment_address,
        sao_luma,
        sao_chroma,
        qp_y,
        cb_qp_offset,
        cr_qp_offset,
        cu_chroma_qp_offset,
        deblocking_disabled,
        beta_offset,
        tc_offset,
        loop_filter_across_slices,
        data_offset: 0,
    })
}

/// POC LSB and the reference picture sets, which an intra decoder does not use.
fn skip_reference_pictures(reader: &mut BitReader<'_>, sps: &Sps) -> Result<(), DecodeError> {
    reader.skip(sps.log2_max_poc_lsb as usize)?;
    let set_count = sps.short_term_sets.len() as u32;
    if !reader.flag()? {
        ShortTermSet::parse(reader, &sps.short_term_sets, true)?;
    } else if set_count > 1 {
        if reader.bits(ceil_log2(set_count))? >= set_count {
            return Err(DecodeError::corrupt("HEVC short-term set index"));
        }
    } else if set_count == 0 {
        return Err(DecodeError::corrupt("HEVC slice refers to a missing short-term set"));
    }
    if let Some(sps_pictures) = sps.long_term_pictures {
        let from_sps =
            if sps_pictures > 0 { reader.ue_max(sps_pictures, "long-term count")? } else { 0 };
        let coded = reader.ue_max(32, "long-term picture count")?;
        for i in 0..from_sps + coded {
            if i < from_sps {
                reader.skip(ceil_log2(sps_pictures) as usize)?;
            } else {
                reader.skip(sps.log2_max_poc_lsb as usize + 1)?;
            }
            if reader.flag()? {
                reader.ue()?;
            }
        }
    }
    if sps.temporal_mvp {
        reader.skip(1)?;
    }
    Ok(())
}

/// Ceil(Log2(value)) for value >= 1.
pub fn ceil_log2(value: u32) -> u32 {
    32 - value.saturating_sub(1).leading_zeros()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ceil_log2_matches_the_spec() {
        assert_eq!([1, 2, 3, 4, 5, 64, 65].map(ceil_log2), [0, 1, 2, 2, 3, 6, 7]);
    }
}
