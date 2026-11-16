//! Module: mirante_format_jpeg::blocks
//! Purpose: Decodes the Huffman-coded coefficients of one 8x8 block per call.
//! Created: 2026-11-16
//! Architecture: Covers baseline blocks and the four progressive passes (ITU T.81
//! G.1.2). Coefficients are stored in natural order, still quantized.

use crate::huffman::{BitReader, HuffmanTable};
use crate::markers::{Scan, Tables, ZIGZAG};
use mirante_core::DecodeError;

/// Largest successive-approximation shift that keeps 8-bit coefficients in i16.
const MAX_POINT_TRANSFORM: u8 = 13;
/// DC differences of 8-bit data need at most 11 bits; 15 still fits in i32 safely.
const MAX_DC_BITS: u8 = 15;
const LAST_COEFFICIENT: usize = 63;
/// Run/size symbol meaning sixteen zeros.
const ZERO_RUN: u8 = 0xF0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pass {
    Baseline,
    DcFirst,
    DcRefine,
    AcFirst,
    AcRefine,
}

pub struct BlockDecoder<'t> {
    pass: Pass,
    start: usize,
    end: usize,
    point_transform: u32,
    /// Per scan component, the tables this pass reads.
    dc: Vec<Option<&'t HuffmanTable>>,
    ac: Vec<Option<&'t HuffmanTable>>,
    predictors: Vec<i32>,
    eob_run: u32,
}

impl<'t> BlockDecoder<'t> {
    pub fn new(scan: &Scan, tables: &'t Tables, progressive: bool) -> Result<Self, DecodeError> {
        let pass = match (progressive, scan.start, scan.high) {
            (false, _, _) => Pass::Baseline,
            (true, 0, 0) => Pass::DcFirst,
            (true, 0, _) => Pass::DcRefine,
            (true, _, 0) => Pass::AcFirst,
            (true, _, _) => Pass::AcRefine,
        };
        let (start, end) =
            if pass == Pass::Baseline { (0, LAST_COEFFICIENT) } else { (scan.start, scan.end) };
        let is_dc = matches!(pass, Pass::DcFirst | Pass::DcRefine);

        if progressive {
            let bad_range = if is_dc { end != 0 } else { end < start || end > LAST_COEFFICIENT };
            if bad_range || (!is_dc && scan.components.len() != 1) {
                return Err(DecodeError::corrupt("bad progressive JPEG spectral selection"));
            }
            if scan.low > MAX_POINT_TRANSFORM {
                return Err(DecodeError::corrupt("bad progressive JPEG approximation"));
            }
        }
        let needs_dc = matches!(pass, Pass::Baseline | Pass::DcFirst);
        let needs_ac = matches!(pass, Pass::Baseline | Pass::AcFirst | Pass::AcRefine);
        let mut dc = Vec::with_capacity(scan.components.len());
        let mut ac = Vec::with_capacity(scan.components.len());

        for component in &scan.components {
            dc.push(if needs_dc { Some(tables.dc(component.dc_slot)?) } else { None });
            ac.push(if needs_ac { Some(tables.ac(component.ac_slot)?) } else { None });
        }
        let point_transform = if progressive { u32::from(scan.low) } else { 0 };
        let predictors = vec![0; scan.components.len()];
        Ok(Self { pass, start, end, point_transform, dc, ac, predictors, eob_run: 0 })
    }

    pub fn restart(&mut self) {
        self.predictors.fill(0);
        self.eob_run = 0;
    }

    /// Decodes the next block of scan component `component` into `block`.
    pub fn decode(
        &mut self,
        reader: &mut BitReader<'_>,
        component: usize,
        block: &mut [i16; 64],
    ) -> Result<(), DecodeError> {
        let (dc, ac) = (self.dc[component], self.ac[component]);
        match (self.pass, dc, ac) {
            (Pass::Baseline, Some(dc), Some(ac)) => {
                *block = [0; 64];
                self.decode_dc(reader, component, dc, block)?;
                self.decode_ac_first(reader, ac, block)
            }
            (Pass::DcFirst, Some(dc), _) => self.decode_dc(reader, component, dc, block),
            (Pass::DcRefine, _, _) => {
                if reader.bit() {
                    block[0] |= 1 << self.point_transform;
                }
                Ok(())
            }
            (Pass::AcFirst, _, Some(ac)) => self.decode_ac_first(reader, ac, block),
            (Pass::AcRefine, _, Some(ac)) => self.decode_ac_refine(reader, ac, block),
            _ => Err(DecodeError::corrupt("missing JPEG Huffman table")),
        }
    }

    fn decode_dc(
        &mut self,
        reader: &mut BitReader<'_>,
        component: usize,
        table: &HuffmanTable,
        block: &mut [i16; 64],
    ) -> Result<(), DecodeError> {
        let size = reader.decode(table)?;
        if size > MAX_DC_BITS {
            return Err(DecodeError::corrupt("bad JPEG DC coefficient"));
        }
        let predictor = &mut self.predictors[component];
        *predictor = predictor.wrapping_add(reader.receive_extend(u32::from(size)));
        block[0] = (*predictor << self.point_transform) as i16;
        Ok(())
    }

    /// Baseline AC coefficients, or a first progressive AC pass with EOB runs.
    fn decode_ac_first(
        &mut self,
        reader: &mut BitReader<'_>,
        table: &HuffmanTable,
        block: &mut [i16; 64],
    ) -> Result<(), DecodeError> {
        if self.eob_run > 0 {
            self.eob_run -= 1;
            return Ok(());
        }
        let mut k = self.start.max(1);

        while k <= self.end {
            let symbol = reader.decode(table)?;
            let (run, size) = (u32::from(symbol >> 4), u32::from(symbol & 15));

            if size == 0 {
                if symbol == ZERO_RUN {
                    k += 16;
                    continue;
                }
                self.eob_run = (1 << run) - 1 + reader.bits(run);
                break;
            }
            k += run as usize;
            if k > LAST_COEFFICIENT {
                return Err(DecodeError::corrupt("JPEG coefficient run past the block"));
            }
            block[ZIGZAG[k]] = (reader.receive_extend(size) << self.point_transform) as i16;
            k += 1;
        }
        Ok(())
    }

    fn decode_ac_refine(
        &mut self,
        reader: &mut BitReader<'_>,
        table: &HuffmanTable,
        block: &mut [i16; 64],
    ) -> Result<(), DecodeError> {
        let bit = 1_i16 << self.point_transform;
        let mut k = self.start;

        if self.eob_run == 0 {
            while k <= self.end {
                let symbol = reader.decode(table)?;
                let (mut run, size) = (i32::from(symbol >> 4), symbol & 15);
                let mut value = 0;

                match size {
                    0 if symbol != ZERO_RUN => {
                        self.eob_run = (1 << run) + reader.bits(run as u32);
                        break;
                    }
                    0 => {}
                    1 => value = if reader.bit() { bit } else { -bit },
                    _ => return Err(DecodeError::corrupt("bad progressive JPEG refinement")),
                }
                // Skip `run` zero coefficients, refining the non-zero ones passed on the way.
                while k <= self.end {
                    let coefficient = &mut block[ZIGZAG[k]];
                    k += 1;
                    if *coefficient != 0 {
                        refine(reader, coefficient, bit);
                    } else if run == 0 {
                        *coefficient = value;
                        break;
                    } else {
                        run -= 1;
                    }
                }
            }
        }
        if self.eob_run > 0 {
            // The rest of this block (and of the next eob_run - 1) only refines.
            for &natural in &ZIGZAG[k.min(self.end + 1)..=self.end] {
                if block[natural] != 0 {
                    refine(reader, &mut block[natural], bit);
                }
            }
            self.eob_run -= 1;
        }
        Ok(())
    }
}

fn refine(reader: &mut BitReader<'_>, coefficient: &mut i16, bit: i16) {
    if reader.bit() && *coefficient & bit == 0 {
        *coefficient = if *coefficient > 0 {
            coefficient.wrapping_add(bit)
        } else {
            coefficient.wrapping_sub(bit)
        };
    }
}
