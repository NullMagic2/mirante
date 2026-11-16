//! Module: mirante_format_heic::grid
//! Purpose: Parses `grid` derived items and stitches their tiles into one image.
//! Created: 2026-11-16
//! Architecture: Tiles arrive already decoded, in row-major `dimg` order, so the
//! same assembly serves HEVC, JPEG and future AV1 tiles.

use crate::planes::{Plane, Planes};
use mirante_codec::{ByteReader, Endian};
use mirante_core::{ColorSpace, DecodeError, Image, Rgba};

const LARGE_FIELDS_FLAG: u8 = 1;

/// ImageGrid: tile counts and the size of the reconstructed image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageGrid {
    pub rows: u32,
    pub columns: u32,
    pub width: u32,
    pub height: u32,
}

impl ImageGrid {
    pub fn parse(data: &[u8]) -> Result<Self, DecodeError> {
        let mut reader = ByteReader::new(data, Endian::Big);
        if reader.u8()? != 0 {
            return Err(DecodeError::unsupported("image grid version"));
        }
        let large = reader.u8()? & LARGE_FIELDS_FLAG != 0;
        let rows = u32::from(reader.u8()?) + 1;
        let columns = u32::from(reader.u8()?) + 1;
        let mut size = || if large { reader.u32() } else { reader.u16().map(u32::from) };
        Ok(Self { rows, columns, width: size()?, height: size()? })
    }

    pub fn tile_count(&self) -> usize {
        (self.rows * self.columns) as usize
    }
}

/// Places equally sized tiles left to right, top to bottom, cutting off the overhang.
/// Tiles are consumed one at a time, so the iterator decides how many are alive at once.
pub fn assemble(
    grid: &ImageGrid,
    tiles: impl IntoIterator<Item = Result<Image, DecodeError>>,
) -> Result<Image, DecodeError> {
    let count = DecodeError::check_size(grid.width, grid.height)?;
    let mut canvas: Option<(Vec<Rgba>, u32, u32, ColorSpace)> = None;
    let mut placed = 0;

    for (index, tile) in tiles.into_iter().enumerate() {
        let tile = tile?;
        if index >= grid.tile_count() {
            return Err(DecodeError::corrupt("image grid has too many tiles"));
        }
        let (pixels, tile_width, tile_height, _) = match &mut canvas {
            Some(canvas) => canvas,
            None => canvas.insert(start_canvas(grid, &tile, count)?),
        };
        if (tile.width(), tile.height()) != (*tile_width, *tile_height) {
            return Err(DecodeError::corrupt("image grid tiles differ in size"));
        }
        let left = (index as u32 % grid.columns) * *tile_width;
        let top = (index as u32 / grid.columns) * *tile_height;
        let visible_width = tile.width().min(grid.width.saturating_sub(left)) as usize;

        for y in 0..tile.height().min(grid.height.saturating_sub(top)) {
            let start = (top + y) as usize * grid.width as usize + left as usize;
            pixels[start..start + visible_width].copy_from_slice(&tile.row(y)[..visible_width]);
        }
        placed += 1;
    }
    match canvas {
        Some((pixels, _, _, color_space)) if placed == grid.tile_count() => {
            Ok(Image::new(grid.width, grid.height, pixels, color_space)?)
        }
        _ => Err(DecodeError::corrupt("image grid is missing tiles")),
    }
}

/// Stitches decoded tiles' planes (each with its item size) into one set of
/// planes covering the grid. Tiles whose size does not align with the chroma
/// subsampling are reported Unsupported, so the caller can go tile by tile.
pub fn assemble_planes(
    grid: &ImageGrid,
    tiles: impl IntoIterator<Item = Result<(Planes, (u32, u32)), DecodeError>>,
) -> Result<Planes, DecodeError> {
    let mut canvas: Option<(Planes, (u32, u32))> = None;
    let mut placed = 0;

    for (index, tile) in tiles.into_iter().enumerate() {
        let (tile, size) = tile?;
        if index >= grid.tile_count() {
            return Err(DecodeError::corrupt("image grid has too many tiles"));
        }
        let (planes, tile_size) = match &mut canvas {
            Some(canvas) => canvas,
            None => canvas.insert((blank_planes(grid, &tile, size)?, size)),
        };
        let fits = size == *tile_size && tile.width() >= size.0 && tile.height() >= size.1;
        let same = tile.format == planes.format && tile.luma.bit_depth == planes.luma.bit_depth;
        if !fits || !same {
            return Err(DecodeError::corrupt("image grid tiles differ"));
        }
        let left = (index as u32 % grid.columns) * size.0;
        let top = (index as u32 / grid.columns) * size.1;
        copy_plane(&mut planes.luma, &tile.luma, (left, top), size);
        let (sx, sy) = planes.format.shifts();
        if let (Some(target), Some(source)) = (&mut planes.chroma, &tile.chroma) {
            for (target, source) in target.iter_mut().zip(source) {
                let chroma_size = (size.0 >> sx, size.1 >> sy);
                copy_plane(target, source, (left >> sx, top >> sy), chroma_size);
            }
        }
        placed += 1;
    }
    match canvas {
        Some((planes, _)) if placed == grid.tile_count() => Ok(planes),
        _ => Err(DecodeError::corrupt("image grid is missing tiles")),
    }
}

fn blank_planes(grid: &ImageGrid, first: &Planes, size: (u32, u32)) -> Result<Planes, DecodeError> {
    let (sx, sy) = first.format.shifts();
    if size.0 % (1 << sx) != 0 || size.1 % (1 << sy) != 0 {
        return Err(DecodeError::unsupported("grid tiles not aligned to chroma samples"));
    }
    let (width, height) = (size.0 * grid.columns, size.1 * grid.rows);
    if width < grid.width || height < grid.height {
        return Err(DecodeError::corrupt("image grid tiles do not cover the image"));
    }
    let plane = |width: u32, height: u32, depth: u8| {
        Plane::new(width, height, depth, vec![0; DecodeError::check_size(width, height)?])
    };
    let luma = plane(width, height, first.luma.bit_depth)?;
    let chroma = match &first.chroma {
        Some([cb, cr]) => Some([
            plane(width >> sx, height >> sy, cb.bit_depth)?,
            plane(width >> sx, height >> sy, cr.bit_depth)?,
        ]),
        None => None,
    };
    Planes::new(first.format, luma, chroma)
}

/// Copies the top-left `size` of `source` into `target` at `at`.
fn copy_plane(target: &mut Plane, source: &Plane, at: (u32, u32), size: (u32, u32)) {
    let width = size.0 as usize;
    for y in 0..size.1 as usize {
        let from = y * source.width as usize;
        let to = (at.1 as usize + y) * target.width as usize + at.0 as usize;
        target.samples[to..to + width].copy_from_slice(&source.samples[from..from + width]);
    }
}

fn start_canvas(
    grid: &ImageGrid,
    first: &Image,
    count: usize,
) -> Result<(Vec<Rgba>, u32, u32, ColorSpace), DecodeError> {
    let (tile_width, tile_height) = (first.width(), first.height());
    let covers = u64::from(tile_width) * u64::from(grid.columns) >= u64::from(grid.width)
        && u64::from(tile_height) * u64::from(grid.rows) >= u64::from(grid.height);
    if !covers {
        return Err(DecodeError::corrupt("image grid tiles do not cover the image"));
    }
    Ok((vec![[0.0; 4]; count], tile_width, tile_height, first.color_space().clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(value: f32) -> Image {
        Image::new(2, 2, vec![[value, 0.0, 0.0, 1.0]; 4], ColorSpace::Srgb).unwrap()
    }

    #[test]
    fn parses_small_and_large_grids() {
        let grid = ImageGrid::parse(&[0, 0, 1, 2, 0, 5, 0, 3]).unwrap();
        assert_eq!(grid, ImageGrid { rows: 2, columns: 3, width: 5, height: 3 });
        let large = ImageGrid::parse(&[0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 9]).unwrap();
        assert_eq!((large.width, large.height), (65536, 9));
    }

    #[test]
    fn stitches_and_crops_tiles() {
        let grid = ImageGrid { rows: 2, columns: 2, width: 3, height: 3 };
        let image = assemble(&grid, (0..4).map(|i| Ok(tile(i as f32)))).unwrap();
        let reds: Vec<f32> = image.pixels().iter().map(|p| p[0]).collect();
        assert_eq!(reds, [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 2.0, 2.0, 3.0]);
    }

    fn planes_tile(value: u16) -> Result<(Planes, (u32, u32)), DecodeError> {
        let plane = |w, h| Plane::new(w, h, 8, vec![value; (w * h) as usize]).unwrap();
        let planes = Planes::new(
            crate::planes::ChromaFormat::Yuv420,
            plane(4, 2),
            Some([plane(2, 1), plane(2, 1)]),
        )?;
        Ok((planes, (4, 2)))
    }

    #[test]
    fn stitches_tile_planes() {
        let grid = ImageGrid { rows: 1, columns: 2, width: 7, height: 2 };
        let planes = assemble_planes(&grid, [planes_tile(10), planes_tile(20)]).unwrap();
        assert_eq!((planes.width(), planes.height()), (8, 2));
        assert_eq!(planes.luma.samples[..8], [10, 10, 10, 10, 20, 20, 20, 20]);
        assert_eq!(planes.chroma.unwrap()[0].samples, [10, 10, 20, 20]);
    }

    #[test]
    fn rejects_missing_or_short_tiles() {
        let grid = ImageGrid { rows: 2, columns: 2, width: 3, height: 3 };
        assert!(assemble(&grid, (0..3).map(|_| Ok(tile(0.0)))).is_err());
        let wide = ImageGrid { width: 5, ..grid };
        assert!(assemble(&wide, (0..4).map(|_| Ok(tile(0.0)))).is_err());
    }
}
