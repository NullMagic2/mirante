//! Module: mirante_format_heic::hevc::picture
//! Purpose: The picture being decoded: sample planes, per-4x4 block metadata,
//! the tile layout and neighbour availability (H.265 6.4.1, 6.5.1).
//! Created: 2026-11-16
//! Architecture: ctu.rs writes samples and metadata, deblock.rs and sao.rs read
//! them afterwards; mod.rs crops the planes into the plugin's `Planes`.

use super::params::{Pps, Sps, TileSpacing};
use mirante_core::DecodeError;

/// Level 6.2 MaxLumaPs (Table A.8): the largest picture any HEVC level allows.
const MAX_LUMA_SAMPLES: u64 = 35_651_584;
/// sqrt(8 * MaxLumaPs), the longest side a conforming picture may have.
const MAX_SIDE: u32 = 16_888;
pub const NOT_DECODED: u32 = u32::MAX;

/// Per-4x4 flags used by the loop filters.
pub mod flags {
    /// A transform or coding block edge on the 8x8 grid along the unit's left side.
    pub const VERTICAL_EDGE: u8 = 1;
    /// The same along the unit's top side.
    pub const HORIZONTAL_EDGE: u8 = 2;
    /// PCM with loop filtering off, or transquant bypass: filters leave samples alone.
    pub const UNFILTERED: u8 = 4;
}

/// One colour component, row by row without padding.
pub struct PlaneBuffer {
    pub width: usize,
    pub height: usize,
    pub bit_depth: u32,
    pub samples: Vec<u16>,
}

impl PlaneBuffer {
    fn new(width: usize, height: usize, bit_depth: u32) -> Self {
        Self { width, height, bit_depth, samples: vec![0; width * height] }
    }
}

/// SAO parameters of one CTB for one component.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SaoComponent {
    /// 0 off, 1 band offset, 2 edge offset.
    pub kind: u8,
    /// Band position or edge offset class.
    pub detail: u8,
    pub offsets: [i16; 4],
}

/// Tile boundaries and the raster/tile scan conversions (6.5.1).
pub struct TileLayout {
    pub column_bounds: Vec<u32>,
    pub rs_to_ts: Vec<u32>,
    pub ts_to_rs: Vec<u32>,
    /// Tile index of each CTB in raster order.
    pub tile_id: Vec<u32>,
}

impl TileLayout {
    fn new(sps: &Sps, pps: &Pps) -> Result<Self, DecodeError> {
        let (width, height) = (sps.width_in_ctbs(), sps.height_in_ctbs());
        let (column_bounds, row_bounds) = match &pps.tiles {
            None => (vec![0, width], vec![0, height]),
            Some(TileSpacing::Uniform { columns, rows }) => {
                (uniform_bounds(*columns, width)?, uniform_bounds(*rows, height)?)
            }
            Some(TileSpacing::Explicit { column_widths, row_heights }) => {
                (explicit_bounds(column_widths, width)?, explicit_bounds(row_heights, height)?)
            }
        };
        let count = (width * height) as usize;
        let mut ts_to_rs = Vec::with_capacity(count);
        let mut tile_id = vec![0; count];
        let mut tile = 0;
        for rows in row_bounds.windows(2) {
            for columns in column_bounds.windows(2) {
                for y in rows[0]..rows[1] {
                    for x in columns[0]..columns[1] {
                        let rs = y * width + x;
                        ts_to_rs.push(rs);
                        tile_id[rs as usize] = tile;
                    }
                }
                tile += 1;
            }
        }
        let mut rs_to_ts = vec![0; count];
        for (ts, &rs) in ts_to_rs.iter().enumerate() {
            rs_to_ts[rs as usize] = ts as u32;
        }
        Ok(Self { column_bounds, rs_to_ts, ts_to_rs, tile_id })
    }

    /// The first CTB column of the tile containing CTB column `x`.
    pub fn column_start(&self, x: u32) -> u32 {
        let index = self.column_bounds.partition_point(|&b| b <= x);
        self.column_bounds[index - 1]
    }
}

fn uniform_bounds(tiles: u32, ctbs: u32) -> Result<Vec<u32>, DecodeError> {
    if tiles > ctbs {
        return Err(DecodeError::corrupt("HEVC has more tiles than CTBs"));
    }
    Ok((0..=tiles).map(|i| i * ctbs / tiles).collect())
}

fn explicit_bounds(sizes: &[u32], ctbs: u32) -> Result<Vec<u32>, DecodeError> {
    let mut bounds = vec![0];
    for &size in sizes {
        let next = bounds[bounds.len() - 1] + size;
        if next >= ctbs {
            return Err(DecodeError::corrupt("HEVC tiles exceed the picture"));
        }
        bounds.push(next);
    }
    bounds.push(ctbs);
    Ok(bounds)
}

pub struct Picture {
    pub planes: Vec<PlaneBuffer>,
    pub width: u32,
    pub height: u32,
    pub log2_ctb: u32,
    pub width_in_ctbs: u32,
    pub height_in_ctbs: u32,
    /// Chroma subsampling as shifts (0 or 1).
    pub chroma_shift: (u32, u32),
    /// Picture width in 4x4 units, the stride of the metadata arrays.
    pub width4: usize,
    pub intra_mode: Vec<u8>,
    pub ct_depth: Vec<u8>,
    pub qp_y: Vec<i8>,
    pub flags: Vec<u8>,
    /// Index of the slice header that decoded each CTB.
    pub ctb_slice: Vec<u32>,
    /// SliceAddrRs of each CTB, or NOT_DECODED.
    pub ctb_slice_address: Vec<u32>,
    pub sao: Vec<[SaoComponent; 3]>,
    pub tiles: TileLayout,
}

impl Picture {
    pub fn new(sps: &Sps, pps: &Pps) -> Result<Self, DecodeError> {
        if u64::from(sps.width) * u64::from(sps.height) > MAX_LUMA_SAMPLES
            || sps.width > MAX_SIDE
            || sps.height > MAX_SIDE
        {
            return Err(DecodeError::unsupported(format!(
                "HEVC picture size {}x{}",
                sps.width, sps.height
            )));
        }
        let (width, height) = (sps.width as usize, sps.height as usize);
        let chroma_shift = sps.chroma_shifts();
        let mut planes = vec![PlaneBuffer::new(width, height, sps.bit_depth_luma)];
        if sps.chroma_format_idc != 0 {
            let (cw, ch) = (width >> chroma_shift.0, height >> chroma_shift.1);
            planes.push(PlaneBuffer::new(cw, ch, sps.bit_depth_chroma));
            planes.push(PlaneBuffer::new(cw, ch, sps.bit_depth_chroma));
        }
        let (width4, height4) = (width.div_ceil(4), height.div_ceil(4));
        let ctbs = (sps.width_in_ctbs() * sps.height_in_ctbs()) as usize;
        Ok(Self {
            planes,
            width: sps.width,
            height: sps.height,
            log2_ctb: sps.log2_ctb,
            width_in_ctbs: sps.width_in_ctbs(),
            height_in_ctbs: sps.height_in_ctbs(),
            chroma_shift,
            width4,
            intra_mode: vec![0; width4 * height4],
            ct_depth: vec![0; width4 * height4],
            qp_y: vec![0; width4 * height4],
            flags: vec![0; width4 * height4],
            ctb_slice: vec![0; ctbs],
            ctb_slice_address: vec![NOT_DECODED; ctbs],
            sao: vec![[SaoComponent::default(); 3]; ctbs],
            tiles: TileLayout::new(sps, pps)?,
        })
    }

    /// Index of the 4x4 unit covering luma sample (x, y).
    pub fn unit(&self, x: u32, y: u32) -> usize {
        (y as usize >> 2) * self.width4 + (x as usize >> 2)
    }

    /// Raster address of the CTB covering luma sample (x, y).
    pub fn ctb_at(&self, x: u32, y: u32) -> usize {
        ((y >> self.log2_ctb) * self.width_in_ctbs + (x >> self.log2_ctb)) as usize
    }

    /// Sets a metadata value on every 4x4 unit of a luma block.
    pub fn fill<T: Copy>(field: &mut [T], width4: usize, x: u32, y: u32, size: u32, value: T) {
        for units in block_rows(field, width4, x, y, size) {
            units.fill(value);
        }
    }

    /// ORs a flag into every 4x4 unit of a luma block.
    pub fn fill_flags(field: &mut [u8], width4: usize, x: u32, y: u32, size: u32, flag: u8) {
        for units in block_rows(field, width4, x, y, size) {
            units.iter_mut().for_each(|unit| *unit |= flag);
        }
    }

    /// 6.4.1 z-scan availability of luma sample (x, y) seen from (cx, cy).
    pub fn available(&self, cx: u32, cy: u32, x: i32, y: i32) -> bool {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return false;
        }
        let (x, y) = (x as u32, y as u32);
        let (current, neighbour) = (self.ctb_at(cx, cy), self.ctb_at(x, y));
        if current == neighbour {
            let mask = (1 << self.log2_ctb) - 1;
            return z_order((x & mask) >> 2, (y & mask) >> 2)
                < z_order((cx & mask) >> 2, (cy & mask) >> 2);
        }
        self.ctb_slice_address[neighbour] != NOT_DECODED
            && self.ctb_slice_address[neighbour] == self.ctb_slice_address[current]
            && self.tiles.tile_id[neighbour] == self.tiles.tile_id[current]
    }
}

/// The rows of 4x4 units a luma block covers, clipped to the picture.
fn block_rows<T>(
    field: &mut [T],
    width4: usize,
    x: u32,
    y: u32,
    size: u32,
) -> impl Iterator<Item = &mut [T]> {
    let (x4, size4) = (x as usize >> 2, (size as usize).div_ceil(4));
    let width = size4.min(width4.saturating_sub(x4));
    field
        .chunks_mut(width4)
        .skip(y as usize >> 2)
        .take(size4)
        .map(move |row| &mut row[x4..x4 + width])
}

/// Morton order of a 4x4 unit inside its CTB, which is the decoding order.
fn z_order(x: u32, y: u32) -> u32 {
    let spread = |mut v: u32| {
        v = (v | (v << 2)) & 0x33;
        (v | (v << 1)) & 0x55
    };
    spread(x) | (spread(y) << 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn z_order_interleaves_coordinates() {
        assert_eq!(
            [(0, 0), (1, 0), (0, 1), (1, 1), (2, 0), (0, 2)].map(|(x, y)| z_order(x, y)),
            [0, 1, 2, 3, 4, 8]
        );
        assert_eq!(z_order(15, 15), 255);
    }

    #[test]
    fn splits_tiles_uniformly() {
        assert_eq!(uniform_bounds(3, 10).unwrap(), [0, 3, 6, 10]);
        assert_eq!(explicit_bounds(&[2, 3], 10).unwrap(), [0, 2, 5, 10]);
        assert!(explicit_bounds(&[5, 5], 10).is_err());
    }
}
