//! Module: mirante_format_heic::hevc::cabac
//! Purpose: The CABAC arithmetic decoder and the I-slice context models (H.265 9.3).
//! Created: 2026-11-16
//! Architecture: `Cabac` reads slice data bytes; `ContextSet` holds every context
//! model plus the Rice statistics, so WPP and dependent slices can save and
//! restore them as one value. Context offsets are named per syntax element.

/// First context of each syntax element in `ContextSet::models`.
pub mod ctx {
    pub const SAO_MERGE: usize = 0;
    pub const SAO_TYPE: usize = 1;
    pub const SPLIT_CU: usize = 2;
    pub const TRANSQUANT_BYPASS: usize = 5;
    pub const PART_MODE: usize = 6;
    pub const PREV_INTRA_LUMA: usize = 7;
    pub const INTRA_CHROMA: usize = 8;
    pub const SPLIT_TRANSFORM: usize = 9;
    pub const CBF_LUMA: usize = 12;
    pub const CBF_CHROMA: usize = 14;
    pub const TRANSFORM_SKIP: usize = 19;
    pub const LAST_X_PREFIX: usize = 21;
    pub const LAST_Y_PREFIX: usize = 39;
    pub const CODED_SUB_BLOCK: usize = 57;
    pub const SIG_COEFF: usize = 61;
    pub const GREATER1: usize = 105;
    pub const GREATER2: usize = 129;
    pub const CU_QP_DELTA: usize = 135;
    pub const CHROMA_QP_OFFSET_FLAG: usize = 137;
    pub const CHROMA_QP_OFFSET_IDX: usize = 138;
    pub const LOG2_RES_SCALE: usize = 139;
    pub const RES_SCALE_SIGN: usize = 147;
    pub const COUNT: usize = 149;
}

/// initValue for initType 0 (I slices), in `ctx` order (Tables 9-5 to 9-37).
const INIT_VALUES: [u8; ctx::COUNT] = [
    153, // sao_merge_left/up_flag
    200, // sao_type_idx
    139, 141, 157, // split_cu_flag
    154, // cu_transquant_bypass_flag
    184, // part_mode
    184, // prev_intra_luma_pred_flag
    63,  // intra_chroma_pred_mode
    153, 138, 138, // split_transform_flag
    111, 141, // cbf_luma
    94, 138, 182, 154, 154, // cbf_cb, cbf_cr
    139, 139, // transform_skip_flag (luma, chroma)
    110, 110, 124, 125, 140, 153, 125, 127, 140, 109, 111, 143, 127, 111, 79, 108, 123, 63, 110,
    110, 124, 125, 140, 153, 125, 127, 140, 109, 111, 143, 127, 111, 79, 108, 123, 63, 91, 171,
    134, 141, // coded_sub_block_flag
    111, 111, 125, 110, 110, 94, 124, 108, 124, 107, 125, 141, 179, 153, 125, 107, 125, 141, 179,
    153, 125, 107, 125, 141, 179, 153, 125, 140, 139, 182, 182, 152, 136, 152, 136, 153, 136, 139,
    111, 136, 139, 111, 141, 111, // sig_coeff_flag
    140, 92, 137, 138, 140, 152, 138, 139, 153, 74, 149, 92, 139, 107, 122, 152, 140, 179, 166,
    182, 140, 227, 122, 197, // coeff_abs_level_greater1_flag
    138, 153, 136, 167, 152, 152, // coeff_abs_level_greater2_flag
    154, 154, // cu_qp_delta_abs
    154, // cu_chroma_qp_offset_flag
    154, // cu_chroma_qp_offset_idx
    154, 154, 154, 154, 154, 154, 154, 154, // log2_res_scale_abs_plus1
    154, 154, // res_scale_sign_flag
];

/// rangeTabLps[pStateIdx][qRangeIdx] (Table 9-52).
const RANGE_LPS: [[u8; 4]; 64] = [
    [128, 176, 208, 240],
    [128, 167, 197, 227],
    [128, 158, 187, 216],
    [123, 150, 178, 205],
    [116, 142, 169, 195],
    [111, 135, 160, 185],
    [105, 128, 152, 175],
    [100, 122, 144, 166],
    [95, 116, 137, 158],
    [90, 110, 130, 150],
    [85, 104, 123, 142],
    [81, 99, 117, 135],
    [77, 94, 111, 128],
    [73, 89, 105, 122],
    [69, 85, 100, 116],
    [66, 80, 95, 110],
    [62, 76, 90, 104],
    [59, 72, 86, 99],
    [56, 69, 81, 94],
    [53, 65, 77, 89],
    [51, 62, 73, 85],
    [48, 59, 69, 80],
    [46, 56, 66, 76],
    [43, 53, 63, 72],
    [41, 50, 59, 69],
    [39, 48, 56, 65],
    [37, 45, 54, 62],
    [35, 43, 51, 59],
    [33, 41, 48, 56],
    [32, 39, 46, 53],
    [30, 37, 43, 50],
    [29, 35, 41, 48],
    [27, 33, 39, 45],
    [26, 31, 37, 43],
    [24, 30, 35, 41],
    [23, 28, 33, 39],
    [22, 27, 32, 37],
    [21, 26, 30, 35],
    [20, 24, 29, 33],
    [19, 23, 27, 31],
    [18, 22, 26, 30],
    [17, 21, 25, 28],
    [16, 20, 23, 27],
    [15, 19, 22, 25],
    [14, 18, 21, 24],
    [14, 17, 20, 23],
    [13, 16, 19, 22],
    [12, 15, 18, 21],
    [12, 14, 17, 20],
    [11, 14, 16, 19],
    [11, 13, 15, 18],
    [10, 12, 15, 17],
    [10, 12, 14, 16],
    [9, 11, 13, 15],
    [9, 11, 12, 14],
    [8, 10, 12, 14],
    [8, 9, 11, 13],
    [7, 9, 11, 12],
    [7, 9, 10, 12],
    [7, 8, 10, 11],
    [6, 8, 9, 11],
    [6, 7, 9, 10],
    [6, 7, 8, 9],
    [2, 2, 2, 2],
];

/// transIdxLps (Table 9-53); the MPS transition is min(state + 1, 62).
const NEXT_STATE_LPS: [u8; 64] = [
    0, 0, 1, 2, 2, 4, 4, 5, 6, 7, 8, 9, 9, 11, 11, 12, 13, 13, 15, 15, 16, 16, 18, 18, 19, 19, 21,
    21, 22, 22, 23, 24, 24, 25, 26, 26, 27, 27, 28, 29, 29, 30, 30, 30, 31, 32, 32, 33, 33, 33, 34,
    34, 35, 35, 35, 36, 36, 36, 37, 37, 37, 38, 38, 63,
];

/// Renormalisation shift after an LPS, indexed by the LPS range >> 3.
const RENORM_SHIFT: [u8; 32] = [
    6, 5, 4, 4, 3, 3, 3, 3, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
];
const MAX_MPS_STATE: u8 = 62;

/// Context models (pStateIdx << 1 | valMps) and StatCoeff for Rice adaptation.
#[derive(Clone)]
pub struct ContextSet {
    pub models: [u8; ctx::COUNT],
    pub rice_stats: [u8; 4],
}

impl ContextSet {
    /// 9.3.2.2 initialisation for the slice QP.
    pub fn new(slice_qp: i32) -> Self {
        let qp = slice_qp.clamp(0, 51);
        let models = INIT_VALUES.map(|init| {
            let slope = i32::from(init >> 4) * 5 - 45;
            let offset = (i32::from(init & 15) << 3) - 16;
            let state = (((slope * qp) >> 4) + offset).clamp(1, 126);
            if state <= 63 { ((63 - state) << 1) as u8 } else { (((state - 64) << 1) | 1) as u8 }
        });
        Self { models, rice_stats: [0; 4] }
    }
}

/// The arithmetic decoding engine, keeping 7 look-ahead bits below the 9-bit offset.
pub struct Cabac<'a> {
    data: &'a [u8],
    position: usize,
    value: u32,
    range: u32,
    bits_needed: i32,
    exhausted: bool,
}

impl<'a> Cabac<'a> {
    pub fn new(data: &'a [u8], position: usize) -> Self {
        let mut cabac =
            Self { data, position, value: 0, range: 0, bits_needed: 0, exhausted: false };
        cabac.restart(position);
        cabac
    }

    /// 9.3.2.5: starts decoding afresh at a byte position (new substream or after PCM).
    pub fn restart(&mut self, position: usize) {
        self.position = position.min(self.data.len());
        self.range = 510;
        self.value = (self.next_byte() << 8) | self.next_byte();
        self.bits_needed = -8;
    }

    /// The first byte not yet consumed; after a terminating bin this is aligned data.
    pub fn position(&self) -> usize {
        self.position
    }

    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Whether decoding ran past the end of the data. A complete slice never does:
    /// its last byte holds the stop bit that ends the final terminating bin.
    pub fn exhausted(&self) -> bool {
        self.exhausted
    }

    /// Past the end the stream reads as zeros; the syntax checks catch the damage.
    fn next_byte(&mut self) -> u32 {
        match self.data.get(self.position) {
            Some(&byte) => {
                self.position += 1;
                u32::from(byte)
            }
            None => {
                self.exhausted = true;
                0
            }
        }
    }

    pub fn decode(&mut self, model: &mut u8) -> bool {
        let state = usize::from(*model >> 1);
        let mps = *model & 1;
        let lps = u32::from(RANGE_LPS[state][((self.range >> 6) & 3) as usize]);
        self.range -= lps;
        let scaled_range = self.range << 7;

        if self.value < scaled_range {
            *model = ((state as u8 + 1).min(MAX_MPS_STATE) << 1) | mps;
            if scaled_range < (256 << 7) {
                self.range = scaled_range >> 6;
                self.value <<= 1;
                self.bits_needed += 1;
                if self.bits_needed == 0 {
                    self.bits_needed = -8;
                    self.value |= self.next_byte();
                }
            }
            return mps == 1;
        }
        self.value -= scaled_range;
        let shift = RENORM_SHIFT[(lps >> 3) as usize];
        self.value <<= shift;
        self.range = lps << shift;
        let next_mps = if state == 0 { 1 - mps } else { mps };
        *model = (NEXT_STATE_LPS[state] << 1) | next_mps;
        self.bits_needed += i32::from(shift);
        if self.bits_needed >= 0 {
            self.value |= self.next_byte() << self.bits_needed;
            self.bits_needed -= 8;
        }
        mps == 0
    }

    pub fn bypass(&mut self) -> bool {
        self.value <<= 1;
        self.bits_needed += 1;
        if self.bits_needed >= 0 {
            self.bits_needed = -8;
            self.value |= self.next_byte();
        }
        let scaled_range = self.range << 7;
        if self.value >= scaled_range {
            self.value -= scaled_range;
            true
        } else {
            false
        }
    }

    /// Fixed-length bypass bins, most significant first.
    pub fn bypass_bits(&mut self, count: u32) -> u32 {
        (0..count).fold(0, |value, _| (value << 1) | u32::from(self.bypass()))
    }

    pub fn terminate(&mut self) -> bool {
        self.range -= 2;
        let scaled_range = self.range << 7;
        if self.value >= scaled_range {
            return true;
        }
        if scaled_range < (256 << 7) {
            self.range = scaled_range >> 6;
            self.value <<= 1;
            self.bits_needed += 1;
            if self.bits_needed == 0 {
                self.bits_needed = -8;
                self.value |= self.next_byte();
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialises_contexts_from_the_slice_qp() {
        // initValue 154 gives preCtxState 64 at any QP: pStateIdx 0 with MPS 1.
        let contexts = ContextSet::new(26);
        assert_eq!(contexts.models[ctx::TRANSQUANT_BYPASS], 1);
        // initValue 63 at QP 26: m = -30, n = 104, preCtxState 55 -> pStateIdx 8, MPS 0.
        assert_eq!(contexts.models[ctx::INTRA_CHROMA], 8 << 1);
    }

    #[test]
    fn terminates_on_a_high_offset() {
        let mut cabac = Cabac::new(&[0xFF, 0xFF, 0x12], 0);
        assert!(cabac.terminate());
        assert_eq!(cabac.position(), 2);
        let mut cabac = Cabac::new(&[0x00, 0x00], 0);
        assert!(!cabac.terminate());
    }
}
