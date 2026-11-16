//! Module: mirante_format_jpeg::scan
//! Purpose: Runs the MCUs of one scan through restart intervals and damage.
//! Created: 2026-11-16
//! Architecture: The DCT and lossless decoders implement McuDecoder; this module
//! owns the bit reader, restart markers and resynchronisation for both.

use crate::huffman::BitReader;
use crate::markers::{RST0, RST7, find_marker};
use mirante_core::DecodeError;

/// Restart markers count modulo 8.
const RESTART_CYCLE: usize = 8;

pub trait McuDecoder {
    fn decode_mcu(&mut self, reader: &mut BitReader<'_>, mcu: usize) -> Result<(), DecodeError>;

    /// Resets predictors before the MCU `next_mcu`, which follows a restart marker.
    fn restart(&mut self, next_mcu: usize);
}

pub struct ScanEnd {
    /// Where to look for the marker that follows the scan.
    pub position: usize,
    pub decoded_mcus: usize,
}

/// Decodes `mcu_count` MCUs from entropy-coded data at `start`. A damaged restart
/// interval is abandoned and decoding resumes after the next restart marker;
/// without restart markers the scan stops at the damage, leaving the rest empty.
pub fn decode_scan(
    data: &[u8],
    start: usize,
    restart_interval: usize,
    mcu_count: usize,
    decoder: &mut impl McuDecoder,
) -> ScanEnd {
    let interval = if restart_interval == 0 { mcu_count } else { restart_interval };
    let mut reader = BitReader::new(data, start);
    let (mut mcu, mut interval_index, mut decoded_mcus) = (0, 0, 0);

    while mcu < mcu_count {
        let end = mcu.saturating_add(interval).min(mcu_count);
        while mcu < end {
            if decoder.decode_mcu(&mut reader, mcu).is_err() || reader.overrun() {
                break;
            }
            mcu += 1;
            decoded_mcus += 1;
        }
        if restart_interval == 0 || end == mcu_count {
            break;
        }
        let Some((number, after)) = next_restart(data, reader.position()) else {
            break;
        };
        // The marker found may close a later interval when data was lost.
        interval_index +=
            (number + RESTART_CYCLE - interval_index % RESTART_CYCLE) % RESTART_CYCLE + 1;
        mcu = interval_index.saturating_mul(interval);
        reader = BitReader::new(data, after);
        decoder.restart(mcu);
    }
    ScanEnd { position: reader.position(), decoded_mcus }
}

/// The number of the next restart marker and the position after it, unless
/// another marker comes first.
fn next_restart(data: &[u8], from: usize) -> Option<(usize, usize)> {
    let (at, marker) = find_marker(data, from)?;
    (RST0..=RST7).contains(&marker).then(|| (usize::from(marker - RST0), at + 2))
}
