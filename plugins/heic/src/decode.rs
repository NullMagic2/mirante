//! Module: mirante_format_heic::decode
//! Purpose: Decodes the primary image of a HEIC/HEIF file into an Image.
//! Created: 2026-11-16
//! Architecture: Walks the parsed container: grid and identity derivations, coded
//! items (HEVC through hevc::decode_intra, JPEG through the JPEG plugin), the alpha
//! auxiliary, colour tagging and finally the transformative properties.

use crate::colour::ColourInfo;
use crate::container::{HeifFile, Item, type_name};
use crate::grid::{self, ImageGrid};
use crate::properties::Property;
use crate::{hevc, transform, ycbcr};
use mirante_core::{ColorSpace, DecodeError, Image};
use std::borrow::Cow;
use std::sync::Arc;

/// Identity items may point at grids; anything deeper is a loop or nonsense.
const MAX_DERIVATION_DEPTH: u32 = 4;

/// Whether an item is decoded for its colour or as an alpha plane (luma only).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Purpose {
    Colour,
    Alpha,
}

pub fn decode(bytes: &[u8]) -> Result<Image, DecodeError> {
    let file = HeifFile::parse(bytes)?;
    decode_image(&file, file.primary, 0)
}

/// Decodes an item as displayed: pixels, alpha, colour space, then geometry.
fn decode_image(file: &HeifFile<'_>, id: u32, depth: u32) -> Result<Image, DecodeError> {
    if depth > MAX_DERIVATION_DEPTH {
        return Err(DecodeError::corrupt("HEIF derived images nest too deeply"));
    }
    let item = file.item(id)?;
    let mut image = if item.item_type == *b"iden" {
        check_essential(file, item)?;
        decode_image(file, single_input(file, item)?, depth + 1)?
    } else {
        let colour = colour_info(file, item);
        let image = decode_pixels(file, item, &colour, Purpose::Colour)?;
        colour.finish(with_alpha(file, id, image)?)?
    };
    for (property, _) in file.properties(item).filter(|(p, _)| p.is_transform()) {
        image = transform::apply(image, property)?;
    }
    Ok(image)
}

/// A coded or grid item as RGB in its coded encoding, with opaque alpha.
fn decode_pixels(
    file: &HeifFile<'_>,
    item: &Item,
    colour: &ColourInfo,
    purpose: Purpose,
) -> Result<Image, DecodeError> {
    if item.item_type != *b"grid" {
        return decode_coded(file, item, colour, purpose);
    }
    check_essential(file, item)?;
    let layout = ImageGrid::parse(&file.item_data(item)?)?;
    let tiles = file.references_from(item.id, *b"dimg");
    if tiles.len() != layout.tile_count() {
        return Err(DecodeError::corrupt("image grid tile count does not match its layout"));
    }
    if purpose == Purpose::Colour {
        match decode_hevc_grid(file, &layout, tiles, colour) {
            Some(Err(DecodeError::Unsupported(_))) | None => {}
            Some(result) => return result,
        }
    }
    let decoded = in_parallel(tiles, |&tile| {
        let tile = file.item(tile)?;
        let tile_colour = colour_info(file, tile).or(colour);
        let image = decode_coded(file, tile, &tile_colour, purpose)?;
        // libheif gives each tile of a grid its own alpha auxiliary.
        match purpose {
            Purpose::Colour => with_alpha(file, tile.id, image),
            Purpose::Alpha => Ok(image),
        }
    });
    grid::assemble(&layout, decoded)
}

/// Stitches a grid of plain HEVC tiles as YCbCr planes and converts it once, so
/// chroma upsampling crosses tile seams as in a single picture. None (or an
/// Unsupported layout) means: decode tile by tile instead.
fn decode_hevc_grid(
    file: &HeifFile<'_>,
    layout: &ImageGrid,
    tiles: &[u32],
    colour: &ColourInfo,
) -> Option<Result<Image, DecodeError>> {
    let nclx = colour.matrix_nclx();
    let plain = |id: u32| {
        file.item(id).is_ok_and(|tile| {
            tile.item_type == *b"hvc1"
                && alpha_item(file, id).is_none()
                && colour_info(file, tile).or(colour).matrix_nclx() == nclx
        })
    };
    if !tiles.iter().all(|&id| plain(id)) {
        return None;
    }
    let planes = in_parallel(tiles, |&id| {
        let tile = file.item(id)?;
        check_essential(file, tile)?;
        let planes = hevc_planes(file, tile)?;
        let size = item_size(file, tile).unwrap_or((planes.width(), planes.height()));
        Ok((planes, size))
    });
    Some(
        grid::assemble_planes(layout, planes)
            .and_then(|planes| ycbcr::to_image(&planes, layout.width, layout.height, &nclx)),
    )
}

fn hevc_planes(file: &HeifFile<'_>, item: &Item) -> Result<crate::planes::Planes, DecodeError> {
    let config = file.properties(item).find_map(|(p, _)| match p {
        Property::Hevc(config) => Some(config),
        _ => None,
    });
    let config = config.ok_or_else(|| DecodeError::corrupt("HEVC item without hvcC"))?;
    hevc::decode_intra(config, &file.item_data(item)?)
}

/// Maps `work` over `items` on all cores, yielding results in order. Only one
/// batch of results (one per core) is alive at a time, which bounds memory.
fn in_parallel<'a, T: Sync, R: Send + 'a>(
    items: &'a [T],
    work: impl Fn(&T) -> R + Sync + 'a,
) -> impl Iterator<Item = R> + 'a {
    let cores = std::thread::available_parallelism().map_or(1, usize::from);
    items.chunks(cores).flat_map(move |batch| {
        let work = &work;
        std::thread::scope(|scope| {
            let handles: Vec<_> =
                batch.iter().map(|item| scope.spawn(move || work(item))).collect();
            handles
                .into_iter()
                .map(|handle| {
                    handle.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic))
                })
                .collect::<Vec<_>>()
        })
    })
}

fn decode_coded(
    file: &HeifFile<'_>,
    item: &Item,
    colour: &ColourInfo,
    purpose: Purpose,
) -> Result<Image, DecodeError> {
    if matches!(&item.item_type, b"hvc1" | b"jpeg") {
        check_essential(file, item)?;
    }
    match &item.item_type {
        b"hvc1" => {
            let planes = hevc_planes(file, item)?;
            let (width, height) =
                item_size(file, item).unwrap_or((planes.width(), planes.height()));
            let nclx = colour.matrix_nclx();
            match purpose {
                Purpose::Colour => ycbcr::to_image(&planes, width, height, &nclx),
                Purpose::Alpha => {
                    let values = ycbcr::luma_values(&planes, width, height, nclx.full_range)?;
                    let pixels = values.into_iter().map(|v| [v, v, v, 1.0]).collect();
                    Ok(Image::new(width, height, pixels, ColorSpace::Srgb)?)
                }
            }
        }
        b"jpeg" => {
            let image = mirante_format_jpeg::decode(&jpeg_stream(file, item)?)?;
            Ok(match purpose {
                Purpose::Colour => image,
                Purpose::Alpha => to_grey(image),
            })
        }
        b"av01" => Err(DecodeError::unsupported("AV1-coded HEIF images")),
        _ => Err(DecodeError::unsupported(format!("HEIF item type '{}'", type_name(item)))),
    }
}

/// A JPEG item, with the shared headers from `jpgC` put in front when present.
fn jpeg_stream<'a>(file: &HeifFile<'a>, item: &Item) -> Result<Cow<'a, [u8]>, DecodeError> {
    let data = file.item_data(item)?;
    let prefix = file.properties(item).find_map(|(p, _)| match p {
        Property::JpegPrefix(prefix) => Some(prefix),
        _ => None,
    });
    Ok(match prefix {
        Some(prefix) => Cow::Owned([prefix.as_slice(), &data].concat()),
        None => data,
    })
}

fn to_grey(mut image: Image) -> Image {
    for pixel in image.pixels_mut() {
        let luma = 0.299 * pixel[0] + 0.587 * pixel[1] + 0.114 * pixel[2];
        *pixel = [luma, luma, luma, 1.0];
    }
    image
}

/// Rejects items that need a property we do not understand.
fn check_essential(file: &HeifFile<'_>, item: &Item) -> Result<(), DecodeError> {
    for (property, essential) in file.properties(item) {
        if let (Property::Other(kind), true) = (property, essential) {
            let name = mirante_codec::isobmff::fourcc_text(*kind);
            return Err(DecodeError::unsupported(format!("essential HEIF property '{name}'")));
        }
    }
    Ok(())
}

fn colour_info(file: &HeifFile<'_>, item: &Item) -> ColourInfo {
    let mut info = ColourInfo::default();
    for (property, _) in file.properties(item) {
        match property {
            Property::Nclx(nclx) => info.nclx = info.nclx.or(Some(*nclx)),
            Property::Icc(icc) if info.icc.is_none() => info.icc = Some(Arc::from(icc.as_slice())),
            _ => {}
        }
    }
    info
}

fn item_size(file: &HeifFile<'_>, item: &Item) -> Option<(u32, u32)> {
    file.properties(item).find_map(|(p, _)| match p {
        Property::Size { width, height } => Some((*width, *height)),
        _ => None,
    })
}

fn single_input(file: &HeifFile<'_>, item: &Item) -> Result<u32, DecodeError> {
    match file.references_from(item.id, *b"dimg") {
        [input] => Ok(*input),
        _ => Err(DecodeError::corrupt("identity item needs exactly one input")),
    }
}

/// The auxiliary item that carries this item's alpha plane, if any.
fn alpha_item(file: &HeifFile<'_>, master: u32) -> Option<u32> {
    file.references_to(master, *b"auxl").find(|&aux| {
        file.item(aux).is_ok_and(|item| file.properties(item).any(|(p, _)| p.is_alpha()))
    })
}

/// Adds the item's alpha auxiliary, if it has one, undoing premultiplication.
fn with_alpha(file: &HeifFile<'_>, id: u32, mut image: Image) -> Result<Image, DecodeError> {
    if let Some(alpha) = alpha_item(file, id) {
        let premultiplied = file.references_from(id, *b"prem").contains(&alpha);
        let alpha = decode_pixels(file, file.item(alpha)?, &ColourInfo::default(), Purpose::Alpha)?;
        attach_alpha(&mut image, &alpha, premultiplied);
    }
    Ok(image)
}

/// Copies alpha into the image, resampling by nearest neighbour if sizes differ.
fn attach_alpha(image: &mut Image, alpha: &Image, premultiplied: bool) {
    let (width, height) = (image.width() as usize, image.height() as usize);
    let (alpha_width, alpha_height) = (alpha.width() as usize, alpha.height() as usize);

    for (index, pixel) in image.pixels_mut().iter_mut().enumerate() {
        let (x, y) = (index % width, index / width);
        let source = (y * alpha_height / height) * alpha_width + x * alpha_width / width;
        let value = alpha.pixels()[source][0];
        if premultiplied && value > 0.0 {
            for channel in &mut pixel[..3] {
                *channel = (*channel / value).min(1.0);
            }
        }
        pixel[3] = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{HeifBuilder, TestItem, full_box, iso_box, ispe};

    fn solid_jpeg(width: u32, height: u32, rgb: [f32; 3]) -> Vec<u8> {
        let pixels = vec![[rgb[0], rgb[1], rgb[2], 1.0]; (width * height) as usize];
        let image = Image::new(width, height, pixels, ColorSpace::Srgb).unwrap();
        mirante_format_jpeg::encode(&image).unwrap()
    }

    fn close(pixel: [f32; 4], expected: [f32; 4]) -> bool {
        pixel.iter().zip(expected).all(|(a, b)| (a - b).abs() < 0.03)
    }

    const RED: [f32; 3] = [0.9, 0.1, 0.1];
    const BLUE: [f32; 3] = [0.1, 0.1, 0.9];

    #[test]
    fn decodes_rotated_grid_of_jpeg_tiles() {
        let mut builder = HeifBuilder { primary: 1, ..Default::default() };
        let urn = b"urn:mpeg:hevc:2015:auxid:1\0";
        builder.properties =
            vec![ispe(12, 8), iso_box(b"irot", &[1]), ispe(8, 8), full_box(b"auxC", 0, 0, urn)];
        let mut grid = TestItem::new(1, b"grid", vec![0, 0, 0, 1, 0, 12, 0, 8], &[1, 2]);
        grid.in_idat = true;
        builder.items = vec![
            grid,
            TestItem::new(2, b"jpeg", solid_jpeg(8, 8, RED), &[3]),
            TestItem::new(3, b"jpeg", solid_jpeg(8, 8, BLUE), &[3]),
            TestItem::new(4, b"jpeg", solid_jpeg(8, 8, [0.5; 3]), &[3, 4]),
        ];
        // Like libheif, the alpha belongs to a tile rather than to the grid.
        builder.references = vec![(*b"dimg", 1, vec![2, 3]), (*b"auxl", 4, vec![2])];
        let image = decode(&builder.build()).unwrap();

        // 12x8 turned anticlockwise: the right-hand blue tile ends up on top.
        assert_eq!((image.width(), image.height()), (8, 12));
        assert!(close(image.pixels()[0], [BLUE[0], BLUE[1], BLUE[2], 1.0]));
        assert!(close(image.pixels()[8 * 11], [RED[0], RED[1], RED[2], 0.5]));
    }

    #[test]
    fn attaches_alpha_auxiliary_image() {
        let mut builder = HeifBuilder { primary: 1, ..Default::default() };
        let urn = b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha\0";
        builder.properties = vec![ispe(8, 8), full_box(b"auxC", 0, 0, urn)];
        builder.items = vec![
            TestItem::new(1, b"jpeg", solid_jpeg(8, 8, RED), &[1]),
            TestItem::new(2, b"jpeg", solid_jpeg(8, 8, [0.5; 3]), &[1, 2]),
        ];
        builder.references = vec![(*b"auxl", 2, vec![1])];
        let image = decode(&builder.build()).unwrap();
        assert!(close(image.pixels()[0], [RED[0], RED[1], RED[2], 0.5]));
    }

    #[test]
    fn reports_unsupported_codecs_and_essential_properties() {
        let mut builder = HeifBuilder { primary: 1, ..Default::default() };
        let hvcc = [
            1, 1, 0x60, 0, 0, 0, 0x90, 0, 0, 0, 0, 0, 0x5D, 0xF0, 0, 0xFC, 0xFD, 0xF8, 0xF8, 0, 0,
            0x0F, 0,
        ];
        builder.properties = vec![ispe(8, 8), iso_box(b"hvcC", &hvcc), iso_box(b"zzzz", &[])];
        builder.items = vec![TestItem::new(1, b"hvc1", vec![0; 4], &[1, 2])];
        let error = decode(&builder.build()).unwrap_err();
        assert!(matches!(error, DecodeError::Corrupt(ref r) if r.contains("HEVC")), "{error}");

        builder.items = vec![TestItem::new(1, b"av01", vec![0; 4], &[1])];
        let error = decode(&builder.build()).unwrap_err();
        assert!(matches!(error, DecodeError::Unsupported(ref r) if r.contains("AV1")), "{error}");

        let mut item = TestItem::new(1, b"jpeg", solid_jpeg(8, 8, RED), &[1]);
        item.properties.push((3, true));
        builder.items = vec![item];
        let error = decode(&builder.build()).unwrap_err();
        assert!(matches!(error, DecodeError::Unsupported(ref r) if r.contains("zzzz")), "{error}");
    }

    #[test]
    fn unpremultiplies_and_resamples_alpha() {
        let pixels = vec![[0.25, 0.5, 0.0, 1.0]; 4];
        let mut image = Image::new(2, 2, pixels, ColorSpace::Srgb).unwrap();
        let alpha = Image::new(1, 1, vec![[0.5, 0.5, 0.5, 1.0]], ColorSpace::Srgb).unwrap();
        attach_alpha(&mut image, &alpha, true);
        assert!(image.pixels().iter().all(|&p| p == [0.5, 1.0, 0.0, 0.5]));
    }
}
