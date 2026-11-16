//! Module: mirante_format_heic::hevc::residual
//! Purpose: residual_coding() (H.265 7.3.8.11): last position, coded sub-blocks,
//! significance, greater-1/2 flags, signs with sign data hiding, Rice remainders.
//! Created: 2026-11-16
//! Architecture: Called by ctu.rs for each coded transform block; fills a
//! coefficient array for transform.rs. Context selection follows 9.3.4.2.

use super::cabac::{Cabac, ContextSet, ctx};
use super::scan::{DIAGONAL, HORIZONTAL, SCANS, VERTICAL};
use mirante_core::DecodeError;

/// ctxIdxMap for sig_coeff_flag in 4x4 blocks, by yC * 4 + xC.
const SIG_CTX_4X4: [u8; 16] = [0, 1, 4, 5, 2, 3, 4, 5, 6, 6, 8, 8, 7, 7, 8, 8];
/// Longest coeff_abs_level_remaining prefix before the value cannot fit 32 bits.
const MAX_REMAINING_PREFIX: u32 = 32;
const MAX_ESCAPE_BITS: u32 = 22;
const MAX_RICE: u32 = 4;

/// What residual_coding needs to know about its block.
#[derive(Clone, Copy)]
pub struct ResidualParams {
    pub log2_size: u32,
    /// 0 luma, 1 Cb, 2 Cr.
    pub component: usize,
    pub scan: usize,
    pub bypass: bool,
    /// transform_skip_flag may be coded for this block.
    pub transform_skip_allowed: bool,
    pub sign_hiding: bool,
    /// Sign hiding is off for implicit RDPCM (intra, transform skip, mode 10 or 26).
    pub rdpcm_mode: bool,
    pub implicit_rdpcm: bool,
    pub transform_skip_context: bool,
    pub persistent_rice: bool,
}

/// The decoded block: whether transform skip was used, and the nonzero extent.
pub struct Coefficients {
    pub transform_skip: bool,
    pub extent: (usize, usize),
}

/// scanIdx for an intra block (8.4.4.1 / 7.4.9.11).
pub fn scan_index(log2_size: u32, mode: u8, luma_like: bool) -> usize {
    if log2_size == 2 || (log2_size == 3 && luma_like) {
        match mode {
            6..=14 => return VERTICAL,
            22..=30 => return HORIZONTAL,
            _ => {}
        }
    }
    DIAGONAL
}

pub fn decode(
    cabac: &mut Cabac<'_>,
    contexts: &mut ContextSet,
    params: &ResidualParams,
    coeffs: &mut [i32],
) -> Result<Coefficients, DecodeError> {
    let log2 = params.log2_size;
    let size = 1_usize << log2;
    coeffs[..size * size].fill(0);
    let luma = params.component == 0;
    let models = &mut contexts.models;

    let transform_skip = params.transform_skip_allowed
        && cabac.decode(&mut models[ctx::TRANSFORM_SKIP + usize::from(!luma)]);
    let prefix_x = last_prefix(cabac, models, ctx::LAST_X_PREFIX, log2, luma);
    let prefix_y = last_prefix(cabac, models, ctx::LAST_Y_PREFIX, log2, luma);
    let (mut last_x, mut last_y) = (last_suffix(cabac, prefix_x), last_suffix(cabac, prefix_y));
    if params.scan == VERTICAL {
        std::mem::swap(&mut last_x, &mut last_y);
    }
    let scan = params.scan;
    let sub_scan = &SCANS[log2 as usize - 2][scan];
    let coeff_scan = &SCANS[2][scan];
    let sub_size = size >> 2;

    // Locate the last sub-block and the last position inside it.
    let last_sub_block = sub_scan[..sub_size * sub_size]
        .iter()
        .position(|&(x, y)| (usize::from(x), usize::from(y)) == (last_x >> 2, last_y >> 2))
        .ok_or_else(|| DecodeError::corrupt("HEVC last coefficient position"))?;
    let last_in_block = coeff_scan[..16]
        .iter()
        .position(|&(x, y)| (usize::from(x), usize::from(y)) == (last_x & 3, last_y & 3))
        .ok_or_else(|| DecodeError::corrupt("HEVC last coefficient position"))?;

    let mut coded_sub_blocks = [0_u8; 64];
    let mut greater1_context = 1_usize;
    let sign_hiding_possible = params.sign_hiding
        && !params.bypass
        && !(params.implicit_rdpcm && transform_skip && params.rdpcm_mode);
    let skip_context = params.transform_skip_context && (transform_skip || params.bypass);
    let rice_class = 2 * usize::from(luma) + usize::from(transform_skip || params.bypass);
    let mut extent = (0, 0);

    for i in (0..=last_sub_block).rev() {
        let (xs, ys) = (usize::from(sub_scan[i].0), usize::from(sub_scan[i].1));
        let right = xs + 1 < sub_size && coded_sub_blocks[ys * 8 + xs + 1] != 0;
        let below = ys + 1 < sub_size && coded_sub_blocks[(ys + 1) * 8 + xs] != 0;
        let mut infer_dc = false;
        let coded = if i < last_sub_block && i > 0 {
            let context = usize::from(right || below) + if luma { 0 } else { 2 };
            infer_dc = true;
            cabac.decode(&mut models[ctx::CODED_SUB_BLOCK + context])
        } else {
            true
        };
        coded_sub_blocks[ys * 8 + xs] = u8::from(coded);
        if !coded {
            continue;
        }

        // Significance, positions listed from the highest scan position down.
        let mut significant = [0_u8; 16];
        let mut count = 0;
        let first = if i == last_sub_block {
            significant[0] = last_in_block as u8;
            count = 1;
            last_in_block as isize - 1
        } else {
            15
        };
        let pattern = usize::from(right) + 2 * usize::from(below);
        for n in (0..=first).rev() {
            let n = n as usize;
            let (xp, yp) = (usize::from(coeff_scan[n].0), usize::from(coeff_scan[n].1));
            if n == 0 && infer_dc {
                significant[count] = 0;
                count += 1;
                break;
            }
            let context = if skip_context {
                if luma { 42 } else { 16 + 27 }
            } else {
                let (xc, yc) = ((xs << 2) + xp, (ys << 2) + yp);
                let sig = sig_context(log2, xc, yc, xp, yp, pattern, (xs, ys), luma, scan);
                if luma { sig } else { 27 + sig }
            };
            if cabac.decode(&mut models[ctx::SIG_COEFF + context]) {
                significant[count] = n as u8;
                count += 1;
                infer_dc = false;
            }
        }
        if count == 0 {
            continue;
        }
        let significant = &significant[..count];

        // Greater-than-one and greater-than-two flags.
        let mut context_set = if i == 0 || !luma { 0 } else { 2 };
        if i != last_sub_block && greater1_context == 0 {
            context_set += 1;
        }
        greater1_context = 1;
        let chroma_offset = if luma { 0 } else { 16 };
        let mut greater1 = [false; 16];
        let mut first_greater1 = None;
        for (k, flag) in greater1.iter_mut().enumerate().take(count.min(8)) {
            let context = context_set * 4 + greater1_context + chroma_offset;
            *flag = cabac.decode(&mut models[ctx::GREATER1 + context]);
            if *flag {
                greater1_context = 0;
                first_greater1.get_or_insert(k);
            } else if greater1_context > 0 && greater1_context < 3 {
                greater1_context += 1;
            }
        }
        let greater2 = first_greater1.is_some_and(|_| {
            let context = context_set + if luma { 0 } else { 4 };
            cabac.decode(&mut models[ctx::GREATER2 + context])
        });

        let hide_sign = sign_hiding_possible
            && usize::from(significant[0]) - usize::from(significant[count - 1]) > 3;
        let sign_count = if hide_sign { count - 1 } else { count };
        // Left-aligned so that bit 31 - k is the sign of the k-th coefficient.
        let signs = cabac.bypass_bits(sign_count as u32) << (32 - sign_count as u32);

        // Remaining absolute levels.
        let mut rice =
            if params.persistent_rice { u32::from(contexts.rice_stats[rice_class]) / 4 } else { 0 };
        let mut first_remaining = true;
        let mut sum = 0_i64;
        for (k, &n) in significant.iter().enumerate() {
            let mut base = 1 + i32::from(greater1[k]);
            if Some(k) == first_greater1 {
                base += i32::from(greater2);
            }
            let threshold = if k < 8 { if Some(k) == first_greater1 { 3 } else { 2 } } else { 1 };
            let mut level = base;
            if base == threshold {
                let remaining = remaining_level(cabac, rice)?;
                if params.persistent_rice && first_remaining {
                    let stat = &mut contexts.rice_stats[rice_class];
                    if remaining >= (3 << (*stat / 4)) {
                        *stat += 1;
                    } else if 2 * remaining < (1 << (*stat / 4)) && *stat > 0 {
                        *stat -= 1;
                    }
                }
                first_remaining = false;
                level = base + remaining as i32;
                if level > 3 * (1 << rice) {
                    rice = (rice + 1).min(MAX_RICE);
                }
            }
            sum += i64::from(level);
            let negative = if hide_sign && k == count - 1 {
                sum % 2 == 1
            } else {
                (signs >> (31 - k)) & 1 == 1
            };
            let (xp, yp) = (
                usize::from(coeff_scan[usize::from(n)].0),
                usize::from(coeff_scan[usize::from(n)].1),
            );
            let (xc, yc) = ((xs << 2) + xp, (ys << 2) + yp);
            coeffs[yc * size + xc] = if negative { -level } else { level };
            extent = (extent.0.max(xc), extent.1.max(yc));
        }
    }
    Ok(Coefficients { transform_skip, extent })
}

/// last_sig_coeff_{x,y}_prefix: truncated unary with position-dependent contexts.
fn last_prefix(
    cabac: &mut Cabac<'_>,
    models: &mut [u8],
    base: usize,
    log2: u32,
    luma: bool,
) -> u32 {
    let (offset, shift) =
        if luma { (3 * (log2 - 2) + ((log2 - 1) >> 2), (log2 + 1) >> 2) } else { (15, log2 - 2) };
    let max = 2 * log2 - 1;
    let mut prefix = 0;
    while prefix < max && cabac.decode(&mut models[base + (offset + (prefix >> shift)) as usize]) {
        prefix += 1;
    }
    prefix
}

/// Combines a last-position prefix with its bypass-coded suffix.
fn last_suffix(cabac: &mut Cabac<'_>, prefix: u32) -> usize {
    if prefix <= 3 {
        return prefix as usize;
    }
    let bits = (prefix >> 1) - 1;
    let suffix = cabac.bypass_bits(bits);
    ((1 << bits) * (2 + (prefix & 1)) + suffix) as usize
}

/// sigCtx for one coefficient (9.3.4.2.5), before the chroma offset of 27.
#[allow(clippy::too_many_arguments)]
fn sig_context(
    log2: u32,
    xc: usize,
    yc: usize,
    xp: usize,
    yp: usize,
    pattern: usize,
    sub_block: (usize, usize),
    luma: bool,
    scan: usize,
) -> usize {
    if log2 == 2 {
        return usize::from(SIG_CTX_4X4[(yc << 2) + xc]);
    }
    if xc + yc == 0 {
        return 0;
    }
    let mut sig = match pattern {
        0 => match xp + yp {
            0 => 2,
            1 | 2 => 1,
            _ => 0,
        },
        1 => 2 - yp.min(2),
        2 => 2 - xp.min(2),
        _ => 2,
    };
    if luma {
        if sub_block != (0, 0) {
            sig += 3;
        }
        sig + if log2 == 3 { if scan == DIAGONAL { 9 } else { 15 } } else { 21 }
    } else {
        sig + if log2 == 3 { 9 } else { 12 }
    }
}

/// coeff_abs_level_remaining (9.3.3.11): Rice prefix, then Exp-Golomb escape.
fn remaining_level(cabac: &mut Cabac<'_>, rice: u32) -> Result<u32, DecodeError> {
    let mut prefix = 0;
    while prefix < MAX_REMAINING_PREFIX && cabac.bypass() {
        prefix += 1;
    }
    if prefix <= 3 {
        return Ok((prefix << rice) + cabac.bypass_bits(rice));
    }
    let extra = prefix - 3;
    if prefix == MAX_REMAINING_PREFIX || extra + rice > MAX_ESCAPE_BITS {
        return Err(DecodeError::corrupt("HEVC coefficient level is too large"));
    }
    let suffix = cabac.bypass_bits(extra + rice);
    Ok((((1 << extra) + 2) << rice) + suffix)
}
