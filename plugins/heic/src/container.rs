//! Module: mirante_format_heic::container
//! Purpose: Reads the HEIF `meta` box: items, their locations, references and properties.
//! Created: 2026-11-16
//! Architecture: Pure container parsing on mirante_codec::isobmff; knows nothing about
//! how items are coded. decode.rs walks the result to build the primary image.

use crate::properties::Property;
use mirante_codec::ByteReader;
use mirante_codec::isobmff::{self, FourCc, FullBox, IsoBox, fourcc_text};
use mirante_core::DecodeError;
use std::borrow::Cow;

const PICTURE_HANDLER: FourCc = *b"pict";
const LARGE_PROPERTY_INDEX_FLAG: u32 = 1;
/// Bounds the per-file tables so a hostile `meta` cannot allocate without limit.
const MAX_ITEMS: u32 = 1 << 16;
const MAX_EXTENTS: u16 = 1 << 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Construction {
    File,
    Idat,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Location {
    construction: Construction,
    /// (offset, length) pairs; a zero length runs to the end of the source.
    extents: Vec<(u64, u64)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PropertyLink {
    pub index: usize,
    pub essential: bool,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub id: u32,
    pub item_type: FourCc,
    pub properties: Vec<PropertyLink>,
    location: Option<Location>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    pub kind: FourCc,
    pub from: u32,
    pub to: Vec<u32>,
}

pub struct HeifFile<'a> {
    data: &'a [u8],
    idat: &'a [u8],
    pub primary: u32,
    pub items: Vec<Item>,
    pub references: Vec<Reference>,
    properties: Vec<Property>,
}

impl<'a> HeifFile<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, DecodeError> {
        let meta = isobmff::boxes(data).require(*b"meta")?.full()?;
        let mut file = Self {
            data,
            idat: &[],
            primary: 0,
            items: Vec::new(),
            references: Vec::new(),
            properties: Vec::new(),
        };
        let mut handler = None;
        let mut locations = Vec::new();
        let mut associations = Vec::new();

        for child in meta.children_after(0)? {
            let child = child?;
            match &child.box_type {
                b"hdlr" => handler = Some(parse_handler(&child.full()?)?),
                b"pitm" => file.primary = parse_primary(&child.full()?)?,
                b"iinf" => file.items = parse_item_infos(&child.full()?)?,
                b"iloc" => locations = parse_locations(&child.full()?)?,
                b"iref" => file.references = parse_references(&child.full()?)?,
                b"idat" => file.idat = child.payload,
                b"iprp" => (file.properties, associations) = parse_properties(&child)?,
                _ => {}
            }
        }
        if handler != Some(PICTURE_HANDLER) {
            return Err(DecodeError::unsupported("HEIF file without a picture handler"));
        }
        for (id, location) in locations {
            if let Some(item) = file.items.iter_mut().find(|item| item.id == id) {
                item.location = Some(location);
            }
        }
        for (id, links) in associations {
            if let Some(item) = file.items.iter_mut().find(|item| item.id == id) {
                item.properties.extend(links);
            }
        }
        file.item(file.primary)?;
        Ok(file)
    }

    pub fn item(&self, id: u32) -> Result<&Item, DecodeError> {
        self.items
            .iter()
            .find(|item| item.id == id)
            .ok_or_else(|| DecodeError::corrupt(format!("HEIF item {id} does not exist")))
    }

    /// The item's properties in association order, with their essential flag.
    pub fn properties(&self, item: &Item) -> impl Iterator<Item = (&Property, bool)> {
        item.properties
            .iter()
            .filter_map(|link| Some((self.properties.get(link.index)?, link.essential)))
    }

    /// Items that `from` points to with a reference of `kind`, in order.
    pub fn references_from(&self, from: u32, kind: FourCc) -> &[u32] {
        self.references
            .iter()
            .find(|r| r.from == from && r.kind == kind)
            .map_or(&[], |r| r.to.as_slice())
    }

    /// Items that point at `to` with a reference of `kind`.
    pub fn references_to(&self, to: u32, kind: FourCc) -> impl Iterator<Item = u32> + '_ {
        self.references.iter().filter(move |r| r.kind == kind && r.to.contains(&to)).map(|r| r.from)
    }

    /// The item's bytes; borrowed when it is stored in a single extent.
    pub fn item_data(&self, item: &Item) -> Result<Cow<'a, [u8]>, DecodeError> {
        let location = item.location.as_ref().ok_or_else(|| {
            DecodeError::corrupt(format!("HEIF item {} has no location", item.id))
        })?;
        let source = match location.construction {
            Construction::File => self.data,
            Construction::Idat => self.idat,
        };
        let slices: Vec<&'a [u8]> = location
            .extents
            .iter()
            .map(|&(offset, length)| extent(source, offset, length))
            .collect::<Result<_, _>>()?;
        if let [single] = slices.as_slice() {
            return Ok(Cow::Borrowed(single));
        }
        let total: usize = slices.iter().map(|s| s.len()).sum();
        if total > self.data.len() {
            return Err(DecodeError::corrupt("HEIF item is larger than the file"));
        }
        Ok(Cow::Owned(slices.concat()))
    }
}

fn extent(source: &[u8], offset: u64, length: u64) -> Result<&[u8], DecodeError> {
    let start = usize::try_from(offset).ok().filter(|&start| start <= source.len());
    let start = start.ok_or_else(|| DecodeError::corrupt("HEIF item extent outside the file"))?;
    let end = match length {
        0 => Some(source.len()),
        length => usize::try_from(length).ok().and_then(|length| start.checked_add(length)),
    };
    end.and_then(|end| source.get(start..end))
        .ok_or_else(|| DecodeError::corrupt("HEIF item extent outside the file"))
}

fn parse_handler(full: &FullBox<'_>) -> Result<FourCc, DecodeError> {
    let mut reader = full.reader();
    reader.skip(4)?;
    Ok(reader.bytes(4)?.try_into().expect("four bytes"))
}

fn parse_primary(full: &FullBox<'_>) -> Result<u32, DecodeError> {
    read_id(&mut full.reader(), full.version != 0)
}

fn parse_item_infos(full: &FullBox<'_>) -> Result<Vec<Item>, DecodeError> {
    let count_size = if full.version == 0 { 2 } else { 4 };
    let mut items = Vec::new();

    for entry in full.children_after(count_size)? {
        let entry = entry?;
        if entry.box_type != *b"infe" {
            continue;
        }
        if items.len() as u32 == MAX_ITEMS {
            return Err(DecodeError::unsupported("too many HEIF items"));
        }
        items.push(parse_item_info(&entry.full()?)?);
    }
    Ok(items)
}

fn parse_item_info(infe: &FullBox<'_>) -> Result<Item, DecodeError> {
    let mut reader = infe.reader();
    let (id, item_type) = match infe.version {
        0 | 1 => (u32::from(reader.u16()?), [0; 4]),
        2 | 3 => {
            let id = read_id(&mut reader, infe.version == 3)?;
            reader.skip(2)?;
            (id, reader.bytes(4)?.try_into().expect("four bytes"))
        }
        version => return Err(DecodeError::unsupported(format!("infe version {version}"))),
    };
    Ok(Item { id, item_type, properties: Vec::new(), location: None })
}

/// Reads an `iloc` field whose width (0, 4 or 8 bytes) the box header declares.
fn sized(reader: &mut ByteReader<'_>, size: u8) -> Result<u64, DecodeError> {
    match size {
        0 => Ok(0),
        4 => reader.u32().map(u64::from),
        8 => reader.u64(),
        _ => Err(DecodeError::corrupt(format!("iloc field size {size}"))),
    }
}

fn parse_locations(full: &FullBox<'_>) -> Result<Vec<(u32, Location)>, DecodeError> {
    let version = full.version;
    if version > 2 {
        return Err(DecodeError::unsupported(format!("iloc version {version}")));
    }
    let mut reader = full.reader();
    let sizes = reader.u16()?;
    let [offset_size, length_size, base_offset_size, low] =
        [12, 8, 4, 0].map(|shift| ((sizes >> shift) & 0xF) as u8);
    let index_size = if version == 0 { 0 } else { low };
    let count = read_id(&mut reader, version >= 2)?;
    if count > MAX_ITEMS {
        return Err(DecodeError::unsupported("too many HEIF item locations"));
    }
    let mut locations = Vec::with_capacity(count as usize);

    for _ in 0..count {
        let id = read_id(&mut reader, version >= 2)?;
        let construction = match version {
            0 => 0,
            _ => reader.u16()? & 0xF,
        };
        let construction = match construction {
            0 => Construction::File,
            1 => Construction::Idat,
            method => {
                return Err(DecodeError::unsupported(format!("iloc construction method {method}")));
            }
        };
        reader.skip(2)?;
        let base = sized(&mut reader, base_offset_size)?;
        let extent_count = reader.u16()?;
        if extent_count > MAX_EXTENTS {
            return Err(DecodeError::unsupported("too many HEIF item extents"));
        }
        let mut extents = Vec::with_capacity(usize::from(extent_count));
        for _ in 0..extent_count {
            sized(&mut reader, index_size)?;
            let offset = base.checked_add(sized(&mut reader, offset_size)?);
            let offset = offset.ok_or_else(|| DecodeError::corrupt("iloc offset overflows"))?;
            extents.push((offset, sized(&mut reader, length_size)?));
        }
        locations.push((id, Location { construction, extents }));
    }
    Ok(locations)
}

fn parse_references(full: &FullBox<'_>) -> Result<Vec<Reference>, DecodeError> {
    let wide = full.version != 0;
    let mut references = Vec::new();

    for child in full.children_after(0)? {
        let child = child?;
        let mut reader = child.reader();
        let from = read_id(&mut reader, wide)?;
        let count = reader.u16()?;
        let to = (0..count).map(|_| read_id(&mut reader, wide)).collect::<Result<_, _>>()?;
        references.push(Reference { kind: child.box_type, from, to });
    }
    Ok(references)
}

/// Item ids are 16 bits wide in version 0 boxes and 32 bits wide later.
fn read_id(reader: &mut ByteReader<'_>, wide: bool) -> Result<u32, DecodeError> {
    if wide { reader.u32() } else { reader.u16().map(u32::from) }
}

type Associations = Vec<(u32, Vec<PropertyLink>)>;

fn parse_properties(iprp: &IsoBox<'_>) -> Result<(Vec<Property>, Associations), DecodeError> {
    let container = iprp.children().require(*b"ipco")?;
    let properties: Vec<Property> =
        container.children().map(|p| Property::parse(&p?)).collect::<Result<_, _>>()?;
    let mut associations = Vec::new();

    for ipma in iprp.children() {
        let ipma = ipma?;
        if ipma.box_type != *b"ipma" {
            continue;
        }
        let ipma = ipma.full()?;
        let mut reader = ipma.reader();
        let count = reader.u32()?;
        if count > MAX_ITEMS {
            return Err(DecodeError::unsupported("too many HEIF property associations"));
        }
        for _ in 0..count {
            let id = read_id(&mut reader, ipma.version >= 1)?;
            let mut links = Vec::new();
            for _ in 0..reader.u8()? {
                links.extend(property_link(&mut reader, ipma.flags)?);
            }
            associations.push((id, links));
        }
    }
    Ok((properties, associations))
}

/// One `ipma` association; index 0 means "no property" and yields None.
fn property_link(
    reader: &mut ByteReader<'_>,
    flags: u32,
) -> Result<Option<PropertyLink>, DecodeError> {
    let (essential, index) = if flags & LARGE_PROPERTY_INDEX_FLAG != 0 {
        let value = reader.u16()?;
        (value & 0x8000 != 0, usize::from(value & 0x7FFF))
    } else {
        let value = reader.u8()?;
        (value & 0x80 != 0, usize::from(value & 0x7F))
    };
    Ok(index.checked_sub(1).map(|index| PropertyLink { index, essential }))
}

/// Names an item type for error messages.
pub fn type_name(item: &Item) -> String {
    fourcc_text(item.item_type)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{HeifBuilder, TestItem, ispe};

    #[test]
    fn parses_items_locations_references_and_properties() {
        let mut builder = HeifBuilder { primary: 1, ..Default::default() };
        builder.properties = vec![ispe(4, 2), crate::testing::iso_box(b"irot", &[1])];
        let mut grid = TestItem::new(1, b"grid", vec![0, 0, 0, 1, 0, 4, 0, 2], &[1, 2]);
        grid.in_idat = true;
        let mut tile = TestItem::new(2, b"jpeg", b"0123456789".to_vec(), &[]);
        tile.split = true;
        builder.items = vec![grid, tile, TestItem::new(3, b"jpeg", b"abc".to_vec(), &[])];
        builder.references = vec![(*b"dimg", 1, vec![2, 3]), (*b"thmb", 3, vec![1])];
        let bytes = builder.build();
        let file = HeifFile::parse(&bytes).unwrap();

        assert_eq!(file.primary, 1);
        let grid = file.item(1).unwrap();
        assert_eq!(grid.item_type, *b"grid");
        assert_eq!(&*file.item_data(grid).unwrap(), &[0, 0, 0, 1, 0, 4, 0, 2]);
        assert_eq!(&*file.item_data(file.item(2).unwrap()).unwrap(), b"0123456789");
        assert_eq!(&*file.item_data(file.item(3).unwrap()).unwrap(), b"abc");
        assert_eq!(file.references_from(1, *b"dimg"), &[2, 3]);
        assert_eq!(file.references_to(1, *b"thmb").collect::<Vec<_>>(), [3]);
        let properties: Vec<_> = file.properties(grid).map(|(p, _)| p.clone()).collect();
        assert_eq!(properties, [Property::Size { width: 4, height: 2 }, Property::Rotation(1)]);
    }

    #[test]
    fn rejects_missing_primary_and_bad_extents() {
        let builder = HeifBuilder {
            primary: 9,
            items: vec![TestItem::new(1, b"jpeg", vec![1], &[])],
            ..Default::default()
        };
        assert!(HeifFile::parse(&builder.build()).is_err());
        let source = [0_u8; 8];
        assert!(extent(&source, 6, 4).is_err());
        assert_eq!(extent(&source, 6, 0).unwrap().len(), 2);
    }
}
