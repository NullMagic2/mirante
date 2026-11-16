//! Module: mirante_raw_canon::crx::header
//! Purpose: Parses CRX image parameters (the CMP1 box) and the tile, plane and
//! subband headers at the start of the raw sample.
//! Created: 2026-11-16
//! Architecture: Produces plain geometry and byte ranges; band.rs, wavelet.rs
//! and mod.rs do the decoding. Layout follows LibRaw's crx.cpp.

use mirante_codec::{ByteReader, Endian};
use mirante_core::DecodeError;

const VERSION_1: u16 = 0x100;
const VERSION_2: u16 = 0x200;
const MIN_TILE_SIDE: usize = 0x16;
const MAX_PLANE_SIDE: usize = 0x7FFF;
const MAX_TILES_PER_SIDE: usize = 0xFF;
const MAX_LEVELS: u8 = 3;
const TILE_SIGN: u16 = 0xFF01;
const TILE_SIGN_V2: u16 = 0xFF11;
const PLANE_SIGNS: [u16; 2] = [0xFF02, 0xFF12];
const BAND_SIGN: u16 = 0xFF03;
const BAND_SIGN_V2: u16 = 0xFF13;
/// Size of a tile header that carries quantisation data, and its tail mark.
const TILE_HEADER_WITH_QP: u16 = 16;
const TILE_TAIL_WITH_QP: u16 = 0x4000;
const PLANE_HEADER_BYTES: usize = 12;

/// The CMP1 box: how the full-resolution image was coded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageHeader {
    pub version: u16,
    pub width: usize,
    pub height: usize,
    pub tile_width: usize,
    pub tile_height: usize,
    pub bits: u32,
    pub planes: usize,
    /// Position of the red plane in the 2x2 cell: 0 RGGB, 1 GRBG, 2 GBRG, 3 BGGR.
    pub cfa_layout: u8,
    pub encoding: u8,
    pub levels: u8,
    /// Bytes of tile, plane and subband headers before the coded data.
    pub header_size: usize,
}

pub fn parse_cmp1(payload: &[u8]) -> Result<ImageHeader, DecodeError> {
    let mut reader = ByteReader::new(payload, Endian::Big);
    reader.skip(4)?;
    let version = reader.u16()?;
    reader.skip(2)?;
    let mut size = || reader.u32().map(|v| v as usize);
    let (width, height, tile_width, tile_height) = (size()?, size()?, size()?, size()?);
    let bits = u32::from(reader.u8()?);
    let packed = reader.u8()?;
    let coding = reader.u8()?;
    reader.skip(1)?;
    let header_size = reader.u32()? as usize;
    let header = ImageHeader {
        version,
        width,
        height,
        tile_width,
        tile_height,
        bits,
        planes: usize::from(packed >> 4),
        cfa_layout: packed & 0xF,
        encoding: coding >> 4,
        levels: coding & 0xF,
        header_size,
    };
    header.validate()?;
    Ok(header)
}

impl ImageHeader {
    fn validate(&self) -> Result<(), DecodeError> {
        if !matches!(self.version, VERSION_1 | VERSION_2) || self.header_size == 0 {
            return Err(DecodeError::unsupported(format!("CRX version {:#x}", self.version)));
        }
        if self.planes != 4 || self.encoding != 0 {
            return Err(DecodeError::unsupported(format!(
                "CRX with {} planes and encoding {}",
                self.planes, self.encoding
            )));
        }
        let even = [self.width, self.height, self.tile_width, self.tile_height];
        if even.iter().any(|side| side % 2 == 1)
            || self.cfa_layout > 3
            || !(9..=14).contains(&self.bits)
        {
            return Err(DecodeError::corrupt("bad CRX image header"));
        }
        if self.tile_width > self.width
            || self.tile_height > self.height
            || self.levels > MAX_LEVELS
        {
            return Err(DecodeError::corrupt("bad CRX tile layout"));
        }
        Ok(())
    }

    /// Each colour plane holds a quarter of the photosites.
    pub fn plane_size(&self) -> (usize, usize) {
        (self.width / 2, self.height / 2)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Band {
    /// Byte range of the coded data within the plane's data.
    pub offset: usize,
    pub size: usize,
    /// Bytes the band occupies, including its unused tail.
    pub stored: usize,
    pub width: usize,
    pub height: usize,
    /// Version 1 quantisation parameter; version 2 uses `q_step_*` and the tile QP map.
    pub q_param: u32,
    pub q_step_base: u32,
    pub q_step_mult: u32,
    pub partial: bool,
}

#[derive(Clone, Debug)]
pub struct PlaneData {
    pub offset: usize,
    pub size: usize,
    pub partial: bool,
    pub rounded_bits: u8,
    pub bands: Vec<Band>,
}

#[derive(Clone, Debug)]
pub struct Tile {
    pub left: usize,
    pub top: usize,
    pub width: usize,
    pub height: usize,
    /// Offset of the tile's data after the headers, and its QP map and padding.
    pub offset: usize,
    pub size: usize,
    pub qp_size: usize,
    pub extra_size: usize,
    pub planes: Vec<PlaneData>,
}

impl Tile {
    /// Where the plane data starts, after the QP map and padding.
    pub fn planes_offset(&self) -> usize {
        self.offset + self.qp_size + self.extra_size
    }
}

/// Splits a plane side into tiles; the last one takes the remainder.
fn tile_spans(total: usize, tile: usize) -> Result<Vec<(usize, usize)>, DecodeError> {
    if tile < MIN_TILE_SIDE || total > MAX_PLANE_SIDE {
        return Err(DecodeError::corrupt("bad CRX tile size"));
    }
    let count = total.div_ceil(tile);
    let last = total - tile * (count - 1);
    if count > MAX_TILES_PER_SIDE || last < MIN_TILE_SIDE {
        return Err(DecodeError::corrupt("bad CRX tile count"));
    }
    Ok((0..count).map(|i| (i * tile, if i + 1 == count { last } else { tile })).collect())
}

/// Subband sizes for one tile without neighbours: per level from the finest,
/// HL (high columns), LH (high rows) and HH, then the remaining LL band.
pub fn band_sizes(width: usize, height: usize, levels: u8) -> Vec<(usize, usize)> {
    let mut sizes = vec![(0, 0); 3 * usize::from(levels) + 1];
    let (mut w, mut h) = (width, height);
    for level in 0..usize::from(levels) {
        let (low_w, low_h) = (w.div_ceil(2), h.div_ceil(2));
        let (high_w, high_h) = (w / 2, h / 2);
        let last = sizes.len() - 1 - 3 * level;
        sizes[last] = (high_w, high_h);
        sizes[last - 1] = (low_w, high_h);
        sizes[last - 2] = (high_w, low_h);
        (w, h) = (low_w, low_h);
    }
    sizes[0] = (w, h);
    sizes
}

/// Reads the tile, plane and subband headers.
pub fn parse_tiles(image: &ImageHeader, data: &[u8]) -> Result<Vec<Tile>, DecodeError> {
    let (plane_width, plane_height) = image.plane_size();
    let columns = tile_spans(plane_width, image.tile_width / 2)?;
    let rows = tile_spans(plane_height, image.tile_height / 2)?;
    if image.levels > 0 && (columns.len() > 1 || rows.len() > 1) {
        // Tiles then borrow coefficients from their neighbours; not written yet.
        return Err(DecodeError::unsupported("tiled lossy CRX (CRAW) images"));
    }
    let headers = data.get(..image.header_size).ok_or_else(truncated)?;
    let mut reader = ByteReader::new(headers, Endian::Big);
    let mut tiles = Vec::with_capacity(columns.len() * rows.len());
    let mut offset = 0;
    for (index, ((top, height), (left, width))) in
        rows.iter().flat_map(|&row| columns.iter().map(move |&column| (row, column))).enumerate()
    {
        let mut tile = read_tile(&mut reader, image, index)?;
        (tile.left, tile.top, tile.width, tile.height) = (left, top, width, height);
        tile.offset = offset;
        offset += tile.size;
        let sizes = band_sizes(width, height, image.levels);
        let mut plane_offset = 0;
        for plane in 0..image.planes {
            let mut data = read_plane(&mut reader, image, plane)?;
            data.offset = plane_offset;
            plane_offset += data.size;
            for (number, &(band_width, band_height)) in sizes.iter().enumerate() {
                let mut band = read_band(&mut reader, number)?;
                (band.width, band.height) = (band_width, band_height);
                data.bands.push(band);
            }
            let mut band_offset = 0;
            for band in &mut data.bands {
                band.offset = band_offset;
                band_offset += band.stored;
            }
            tile.planes.push(data);
        }
        tiles.push(tile);
    }
    Ok(tiles)
}

fn truncated() -> DecodeError {
    DecodeError::corrupt("CRX headers are truncated")
}

fn read_tile(
    reader: &mut ByteReader<'_>,
    image: &ImageHeader,
    index: usize,
) -> Result<Tile, DecodeError> {
    let sign = reader.u16()?;
    let size = reader.u16()?;
    let valid = (sign == TILE_SIGN && size == 8)
        || (sign == TILE_SIGN_V2 && (size == 8 || size == TILE_HEADER_WITH_QP));
    let tile_size = reader.u32()? as usize;
    let number = reader.u16()?;
    let tail = reader.u16()?;
    let tail_ok = if size == TILE_HEADER_WITH_QP { tail == TILE_TAIL_WITH_QP } else { tail == 0 };
    if !valid || !tail_ok || usize::from(number) != index {
        return Err(DecodeError::corrupt("bad CRX tile header"));
    }
    let (mut qp_size, mut extra_size) = (0, 0);
    if size == TILE_HEADER_WITH_QP {
        qp_size = reader.u32()? as usize;
        extra_size = usize::from(reader.u16()?);
        if reader.u16()? != 0 || image.version != VERSION_2 {
            return Err(DecodeError::corrupt("bad CRX tile header"));
        }
    }
    Ok(Tile {
        left: 0,
        top: 0,
        width: 0,
        height: 0,
        offset: 0,
        size: tile_size,
        qp_size,
        extra_size,
        planes: Vec::new(),
    })
}

fn read_plane(
    reader: &mut ByteReader<'_>,
    image: &ImageHeader,
    plane: usize,
) -> Result<PlaneData, DecodeError> {
    let header = reader.bytes(PLANE_HEADER_BYTES)?;
    let field = |at: usize| u16::from_be_bytes([header[at], header[at + 1]]);
    let size = u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize;
    let flags = header[8];
    if !PLANE_SIGNS.contains(&field(0))
        || field(2) != 8
        || usize::from(flags >> 4) != plane
        || header[9..12] != [0, 0, 0]
    {
        return Err(DecodeError::corrupt("bad CRX plane header"));
    }
    let partial = flags & 8 != 0;
    let rounded_bits = (flags >> 1) & 3;
    if rounded_bits != 0 && (image.levels > 0 || !partial) {
        return Err(DecodeError::corrupt("bad CRX plane header"));
    }
    Ok(PlaneData { offset: 0, size, partial, rounded_bits, bands: Vec::new() })
}

fn read_band(reader: &mut ByteReader<'_>, number: usize) -> Result<Band, DecodeError> {
    let sign = reader.u16()?;
    let header_size = reader.u16()?;
    if !((sign == BAND_SIGN && header_size == 8) || (sign == BAND_SIGN_V2 && header_size == 16)) {
        return Err(DecodeError::corrupt("bad CRX subband header"));
    }
    let size = reader.u32()? as usize;
    let mut band = Band { size, stored: size, ..Band::default() };
    if sign == BAND_SIGN {
        let fields = reader.u32()?;
        if (fields >> 28) as usize != number {
            return Err(DecodeError::corrupt("CRX subbands are out of order"));
        }
        band.size = size.saturating_sub((fields & 0x7FFFF) as usize);
        band.partial = fields & 0x0800_0000 != 0;
        band.q_param = (fields >> 19) & 0xFF;
    } else {
        let flags = reader.u16()?;
        band.q_step_mult = u32::from(reader.u16()?);
        band.q_step_base = reader.u32()?;
        let padding = usize::from(reader.u16()?);
        if usize::from(flags >> 12) != number || flags & 0xFFF != 0 || reader.u16()? != 0 {
            return Err(DecodeError::corrupt("bad CRX subband header"));
        }
        band.size = size.saturating_sub(padding);
    }
    Ok(band)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A version 1 CMP1 payload for a lossless image.
    pub fn cmp1(width: u32, height: u32, header_size: u32) -> Vec<u8> {
        let mut out = vec![0xFF, 0, 0, 0x30, 0x01, 0x00, 0, 0];
        for value in [width, height, width, height] {
            out.extend(value.to_be_bytes());
        }
        out.extend([14, 0x40, 0x00, 0x00]);
        out.extend(header_size.to_be_bytes());
        out.extend([0; 20]);
        out
    }

    /// Tile, plane and subband headers for one tile of four single-band planes.
    pub fn headers(band_bytes: u32) -> Vec<u8> {
        let mut out = vec![0xFF, 0x01, 0, 8];
        out.extend((4 * band_bytes).to_be_bytes());
        out.extend([0, 0, 0, 0]);
        for plane in 0..4_u8 {
            out.extend([0xFF, 0x02, 0, 8]);
            out.extend(band_bytes.to_be_bytes());
            out.extend([plane << 4 | 8, 0, 0, 0]);
            out.extend([0xFF, 0x03, 0, 8]);
            out.extend(band_bytes.to_be_bytes());
            out.extend([0, 0, 0, 0]);
        }
        out
    }

    #[test]
    fn parses_image_and_tile_headers() {
        let image = parse_cmp1(&cmp1(48, 48, 108)).unwrap();
        assert_eq!((image.width, image.planes, image.levels, image.header_size), (48, 4, 0, 108));
        let tiles = parse_tiles(&image, &headers(64)).unwrap();
        assert_eq!(tiles.len(), 1);
        assert_eq!((tiles[0].width, tiles[0].height), (24, 24));
        assert_eq!(tiles[0].planes[3].offset, 192);
        assert!(tiles[0].planes.iter().all(|plane| plane.partial && plane.bands[0].size == 64));
        let mut odd = cmp1(48, 48, 108);
        odd[11] = 47;
        assert!(parse_cmp1(&odd).is_err());
    }

    #[test]
    fn sizes_subbands_per_level() {
        let sizes = band_sizes(1792, 1193, 3);
        assert_eq!(sizes[0], (224, 150));
        assert_eq!(&sizes[1..4], [(224, 150), (224, 149), (224, 149)]);
        assert_eq!(&sizes[7..], [(896, 597), (896, 596), (896, 596)]);
    }
}
