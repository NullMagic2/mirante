//! Module: mirante_raw_canon::crx
//! Purpose: Decodes Canon's CRX raw codec (CR3): lossless "RAW" and lossy
//! wavelet "C-RAW", four colour planes recombined into a Bayer mosaic.
//! Created: 2026-11-16
//! Architecture: header.rs reads the layout, band.rs the entropy-coded lines,
//! quant.rs and wavelet.rs undo the lossy transform. Each (tile, plane) pair is
//! independent, so they decode in parallel. Ported from LibRaw's crx.cpp by
//! Alexey Danilchenko (LGPL), building on Laurent Clévy's CR3 description.

mod band;
mod bits;
pub mod header;
mod quant;
mod wavelet;

use band::{BandDecoder, Coding};
use header::{Band, ImageHeader, PlaneData, Tile};
use mirante_core::DecodeError;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use wavelet::Block;

/// A decoded CRX image: one sample per photosite, in rows.
pub struct Mosaic {
    pub width: usize,
    pub height: usize,
    pub samples: Vec<u16>,
}

/// QP map cells span 8 plane columns at the finest level, 1 at the deepest.
const QP_MAP_DEPTH: u32 = 3;

pub fn decode(image: &ImageHeader, sample: &[u8]) -> Result<Mosaic, DecodeError> {
    DecodeError::check_size(image.width as u32, image.height as u32)?;
    let tiles = header::parse_tiles(image, sample)?;
    let data = &sample[image.header_size..];
    let jobs: Vec<(usize, usize)> = (0..tiles.len())
        .flat_map(|tile| (0..image.planes).map(move |plane| (tile, plane)))
        .collect();
    let results = run_parallel(jobs.len(), |job| {
        let (tile, plane) = jobs[job];
        decode_tile_plane(image, &tiles[tile], plane, data)
    });
    let mut samples = vec![0; image.width * image.height];
    for (&(tile, plane), result) in jobs.iter().zip(results) {
        let values = result?;
        let tile = &tiles[tile];
        let (dx, dy) = plane_offset(plane, image.cfa_layout);
        for (y, row) in values.chunks_exact(tile.width).enumerate() {
            let start = (2 * (tile.top + y) + dy) * image.width + 2 * tile.left + dx;
            for (x, &value) in row.iter().enumerate() {
                samples[start + 2 * x] = value;
            }
        }
    }
    Ok(Mosaic { width: image.width, height: image.height, samples })
}

/// Where a plane's samples sit in each 2x2 cell. Planes are R, G, G, B; the
/// layout moves red to another corner by mirroring the cell.
pub fn plane_offset(plane: usize, layout: u8) -> (usize, usize) {
    let layout = usize::from(layout);
    ((plane & 1) ^ (layout & 1), (plane >> 1) ^ (layout >> 1))
}

/// Runs `count` jobs on all cores and returns their results in order.
fn run_parallel<T: Send>(count: usize, job: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let threads = thread::available_parallelism().map_or(1, usize::from).min(count).max(1);
    let next = AtomicUsize::new(0);
    let mut results: Vec<(usize, T)> = thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        if index >= count {
                            return done;
                        }
                        done.push((index, job(index)));
                    }
                })
            })
            .collect();
        workers.into_iter().flat_map(|worker| worker.join().expect("CRX worker panicked")).collect()
    });
    results.sort_by_key(|&(index, _)| index);
    results.into_iter().map(|(_, result)| result).collect()
}

/// One plane of one tile, as clipped sample values.
fn decode_tile_plane(
    image: &ImageHeader,
    tile: &Tile,
    plane: usize,
    data: &[u8],
) -> Result<Vec<u16>, DecodeError> {
    let part = &tile.planes[plane];
    if part.rounded_bits != 0 {
        return Err(DecodeError::unsupported("CRX planes with rounded bits"));
    }
    let start = tile.planes_offset() + part.offset;
    // Bands may read past their own data (see `decode_band`), so keep the rest.
    let bytes = data.get(start..).filter(|rest| rest.len() >= part.size).ok_or_else(outside)?;
    let values = if image.levels == 0 {
        decode_band(&part.bands[0], part, 0, bytes)?.values
    } else {
        let maps = match tile.qp_size {
            0 => None,
            _ => {
                let qp = data.get(tile.offset..).filter(|qp| qp.len() >= tile.qp_size);
                let qp = qp.ok_or_else(outside)?;
                Some(quant::step_maps(qp, tile.width, tile.height, image.levels)?)
            }
        };
        inverse_transform(image.levels, part, bytes, maps.as_deref())?
    };
    if values.len() != tile.width * tile.height {
        return Err(DecodeError::corrupt("CRX tile size does not match its subbands"));
    }
    let median = 1_i32 << (image.bits - 1);
    let maximum = (1_i32 << image.bits) - 1;
    Ok(values.iter().map(|&v| median.wrapping_add(v).clamp(0, maximum) as u16).collect())
}

fn outside() -> DecodeError {
    DecodeError::corrupt("CRX data lies outside the raw sample")
}

/// Entropy-decodes a whole subband; empty bands are all zero.
fn decode_band(
    band: &Band,
    plane: &PlaneData,
    number: usize,
    bytes: &[u8],
) -> Result<Block, DecodeError> {
    let mut block =
        Block { width: band.width, height: band.height, values: vec![0; band.width * band.height] };
    if band.size == 0 || band.width == 0 {
        return Ok(block);
    }
    // Canon's decoder reads ahead in blocks, and streams rely on it: their last
    // codes may run on into the bytes that follow.
    let coded =
        bytes.get(band.offset..).filter(|rest| rest.len() >= band.size).ok_or_else(outside)?;
    let coding = if number == 0 && plane.partial { Coding::Predicted } else { Coding::Unpredicted };
    let mut decoder = BandDecoder::new(coded, band.width, coding);
    for line in block.values.chunks_exact_mut(band.width) {
        decoder.decode_line(line)?;
    }
    Ok(block)
}

/// Decodes and dequantises every subband, then undoes the wavelet levels.
fn inverse_transform(
    levels: u8,
    plane: &PlaneData,
    bytes: &[u8],
    maps: Option<&[quant::StepMap]>,
) -> Result<Vec<i32>, DecodeError> {
    let mut bands = Vec::with_capacity(plane.bands.len());
    for (number, band) in plane.bands.iter().enumerate() {
        if band.partial && maps.is_none() {
            return Err(DecodeError::unsupported("CRX subbands with progressive quantisation"));
        }
        let mut block = decode_band(band, plane, number, bytes)?;
        let level = number.saturating_sub(1) / 3;
        let map = maps.map(|maps| &maps[level]);
        let shift = QP_MAP_DEPTH - (u32::from(levels) - level as u32);
        if block.width > 0 {
            for (row, values) in block.values.chunks_exact_mut(block.width).enumerate() {
                quant::dequantize_row(values, band, map, row, shift)?;
            }
        }
        bands.push(block);
    }
    let mut low = bands[0].clone();
    for level in 0..usize::from(levels) {
        let [hl, lh, hh] = [1, 2, 3].map(|i| &bands[3 * level + i]);
        low = wavelet::inverse_level(&low, hl, lh, hh)
            .ok_or_else(|| DecodeError::corrupt("CRX subband sizes do not fit together"))?;
    }
    Ok(low.values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plane_offsets_follow_the_cfa_layout() {
        let cell = |layout| (0..4).map(|p| plane_offset(p, layout)).collect::<Vec<_>>();
        assert_eq!(cell(0), [(0, 0), (1, 0), (0, 1), (1, 1)]);
        assert_eq!(cell(1), [(1, 0), (0, 0), (1, 1), (0, 1)]);
        assert_eq!(cell(3), [(1, 1), (0, 1), (1, 0), (0, 0)]);
    }

    #[test]
    fn decodes_a_lossless_image() {
        // All-ones band data decodes as whole-line runs of zero, i.e. the median.
        let image = header::parse_cmp1(&header::tests::cmp1(48, 48, 108)).unwrap();
        let mut sample = header::tests::headers(64);
        sample.extend([0xFF; 4 * 64]);
        let mosaic = decode(&image, &sample).unwrap();
        assert_eq!((mosaic.width, mosaic.height), (48, 48));
        assert!(mosaic.samples.iter().all(|&v| v == 1 << 13));
        assert!(decode(&image, &sample[..200]).is_err());
    }

    #[test]
    fn runs_jobs_in_order() {
        assert_eq!(run_parallel(5, |i| i * 2), [0, 2, 4, 6, 8]);
    }
}
