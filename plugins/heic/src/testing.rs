//! Module: mirante_format_heic::testing
//! Purpose: Builds small HEIF files box by box inside tests.
//! Created: 2026-11-16
//! Architecture: Test-only. Items live in `mdat` (or `idat`); `iloc` offsets are
//! patched once the size of `meta` is known.

pub fn iso_box(box_type: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend(box_type);
    out.extend(payload);
    out
}

pub fn full_box(box_type: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut body = (u32::from(version) << 24 | flags).to_be_bytes().to_vec();
    body.extend(payload);
    iso_box(box_type, &body)
}

pub struct TestItem {
    pub id: u16,
    pub item_type: [u8; 4],
    pub data: Vec<u8>,
    /// Store in `idat` (construction method 1) instead of `mdat`.
    pub in_idat: bool,
    /// Store as two extents to exercise concatenation.
    pub split: bool,
    /// (property index from 1, essential)
    pub properties: Vec<(u8, bool)>,
}

impl TestItem {
    pub fn new(id: u16, item_type: &[u8; 4], data: Vec<u8>, properties: &[u8]) -> Self {
        let properties = properties.iter().map(|&p| (p, false)).collect();
        Self { id, item_type: *item_type, data, in_idat: false, split: false, properties }
    }
}

#[derive(Default)]
pub struct HeifBuilder {
    pub primary: u16,
    pub items: Vec<TestItem>,
    /// Complete property boxes, referenced by 1-based index.
    pub properties: Vec<Vec<u8>>,
    /// (type, from, to)
    pub references: Vec<([u8; 4], u16, Vec<u16>)>,
}

impl HeifBuilder {
    pub fn build(&self) -> Vec<u8> {
        let ftyp = iso_box(b"ftyp", b"heic\0\0\0\0mif1heic");
        let placeholder = self.meta(0);
        let mdat_start = ftyp.len() + placeholder.len() + 8;
        let meta = self.meta(mdat_start as u32);
        let mdat: Vec<u8> =
            self.items.iter().filter(|i| !i.in_idat).flat_map(|i| i.data.clone()).collect();
        [ftyp, meta, iso_box(b"mdat", &mdat)].concat()
    }

    fn meta(&self, mdat_start: u32) -> Vec<u8> {
        let hdlr = full_box(b"hdlr", 0, 0, b"\0\0\0\0pict\0\0\0\0\0\0\0\0\0\0\0\0\0");
        let pitm = full_box(b"pitm", 0, 0, &self.primary.to_be_bytes());
        let mut iinf = (self.items.len() as u16).to_be_bytes().to_vec();
        for item in &self.items {
            let mut infe = item.id.to_be_bytes().to_vec();
            infe.extend([0, 0]);
            infe.extend(item.item_type);
            infe.push(0);
            iinf.extend(full_box(b"infe", 2, 0, &infe));
        }
        let mut iloc = vec![0x44, 0x00];
        iloc.extend((self.items.len() as u16).to_be_bytes());
        let (mut file_offset, mut idat_offset, mut idat) = (mdat_start, 0_u32, Vec::new());
        for item in &self.items {
            iloc.extend(item.id.to_be_bytes());
            iloc.extend(u16::from(item.in_idat).to_be_bytes());
            iloc.extend([0, 0]);
            let offset = if item.in_idat { &mut idat_offset } else { &mut file_offset };
            let length = item.data.len() as u32;
            let extents =
                if item.split { vec![length / 2, length - length / 2] } else { vec![length] };
            iloc.extend((extents.len() as u16).to_be_bytes());
            for extent in extents {
                iloc.extend(offset.to_be_bytes());
                iloc.extend(extent.to_be_bytes());
                *offset += extent;
            }
            if item.in_idat {
                idat.extend(&item.data);
            }
        }
        let mut iref = Vec::new();
        for (kind, from, to) in &self.references {
            let mut body = from.to_be_bytes().to_vec();
            body.extend((to.len() as u16).to_be_bytes());
            to.iter().for_each(|id| body.extend(id.to_be_bytes()));
            iref.extend(iso_box(kind, &body));
        }
        let mut ipma = (self.items.len() as u32).to_be_bytes().to_vec();
        for item in &self.items {
            ipma.extend(item.id.to_be_bytes());
            ipma.push(item.properties.len() as u8);
            ipma.extend(
                item.properties.iter().map(|&(index, essential)| index | u8::from(essential) << 7),
            );
        }
        let iprp =
            [iso_box(b"ipco", &self.properties.concat()), full_box(b"ipma", 0, 0, &ipma)].concat();
        let children = [
            hdlr,
            pitm,
            full_box(b"iinf", 0, 0, &iinf),
            full_box(b"iloc", 1, 0, &iloc),
            full_box(b"iref", 0, 0, &iref),
            iso_box(b"iprp", &iprp),
            iso_box(b"idat", &idat),
        ];
        full_box(b"meta", 0, 0, &children.concat())
    }
}

pub fn ispe(width: u32, height: u32) -> Vec<u8> {
    full_box(b"ispe", 0, 0, &[width.to_be_bytes(), height.to_be_bytes()].concat())
}
