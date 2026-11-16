//! Module: mirante_format_psd::sections
//! Purpose: Reads the PSD/PSB header, image resources and layer-section facts.
//! Created: 2026-11-16
//! Architecture: Only what the merged composite needs is extracted: the header,
//! the ICC profile, merged-data and transparency hints, and the layer count sign.

use mirante_codec::{ByteReader, Endian};
use mirante_core::DecodeError;

pub const SIGNATURE: &[u8] = b"8BPS";
pub const VERSION_PSD: u16 = 1;
pub const VERSION_PSB: u16 = 2;
const MAX_CHANNELS: usize = 56;
const MAX_SIDE_PSD: u32 = 30_000;
const MAX_SIDE_PSB: u32 = 300_000;

mod resource {
    pub const ALPHA_NAMES: u16 = 0x03EE;
    pub const ICC_PROFILE: u16 = 0x040F;
    pub const UNICODE_ALPHA_NAMES: u16 = 0x0415;
    pub const TRANSPARENCY_INDEX: u16 = 0x0417;
    pub const VERSION_INFO: u16 = 0x0421;
}

/// Tagged blocks that hold the layer info of 16- and 32-bit documents.
const LAYER_INFO_KEYS: [&[u8; 4]; 3] = [b"Layr", b"Lr16", b"Lr32"];
/// PSB tagged blocks whose length field is 8 bytes instead of 4.
const LONG_LENGTH_KEYS: [&[u8; 4]; 13] = [
    b"LMsk", b"Lr16", b"Lr32", b"Layr", b"Mt16", b"Mt32", b"Mtrn", b"Alph", b"FMsk", b"lnk2",
    b"FEid", b"FXid", b"PxSD",
];
const MAX_TAGGED_BLOCKS: usize = 64;

#[derive(Debug)]
pub struct Header {
    pub version: u16,
    pub channels: usize,
    pub height: u32,
    pub width: u32,
    pub depth: u16,
    pub mode: u16,
}

#[derive(Debug, Default)]
pub struct Resources<'a> {
    pub icc: Option<&'a [u8]>,
    /// False when the file was saved without "Maximize Compatibility".
    pub has_merged_data: bool,
    pub transparency_index: Option<u16>,
    /// Extra channels have names, so they are saved selections, not transparency.
    pub named_alpha_channels: bool,
}

impl Header {
    pub fn read(reader: &mut ByteReader) -> Result<Self, DecodeError> {
        if reader.bytes(SIGNATURE.len())? != SIGNATURE {
            return Err(DecodeError::corrupt("not a Photoshop file"));
        }
        let version = reader.u16()?;
        if version != VERSION_PSD && version != VERSION_PSB {
            return Err(DecodeError::unsupported(format!("Photoshop file version {version}")));
        }
        reader.skip(6)?;
        let channels = usize::from(reader.u16()?);
        let (height, width) = (reader.u32()?, reader.u32()?);
        let (depth, mode) = (reader.u16()?, reader.u16()?);
        let max_side = if version == VERSION_PSB { MAX_SIDE_PSB } else { MAX_SIDE_PSD };
        if !(1..=MAX_CHANNELS).contains(&channels) || width > max_side || height > max_side {
            return Err(DecodeError::corrupt("bad Photoshop header"));
        }
        DecodeError::check_size(width, height)?;
        Ok(Self { version, channels, height, width, depth, mode })
    }

    pub fn is_psb(&self) -> bool {
        self.version == VERSION_PSB
    }

    /// Section lengths and RLE row sizes are twice as wide in PSB files.
    pub fn read_length(&self, reader: &mut ByteReader) -> Result<usize, DecodeError> {
        let length = if self.is_psb() { reader.u64()? } else { u64::from(reader.u32()?) };
        usize::try_from(length).map_err(|_| DecodeError::corrupt("Photoshop length overflows"))
    }
}

/// Reads a section that starts with its own 4-byte length.
pub fn section<'a>(reader: &mut ByteReader<'a>) -> Result<&'a [u8], DecodeError> {
    let length = reader.u32()? as usize;
    reader.bytes(length)
}

impl<'a> Resources<'a> {
    pub fn read(data: &'a [u8]) -> Result<Self, DecodeError> {
        let mut reader = ByteReader::new(data, Endian::Big);
        let mut resources = Self { has_merged_data: true, ..Self::default() };

        while reader.remaining() > 0 {
            reader.skip(4)?;
            let id = reader.u16()?;
            let name_length = usize::from(reader.u8()?);
            // The Pascal name, with its length byte, is padded to an even size.
            reader.skip(name_length + (name_length + 1) % 2)?;
            let size = reader.u32()? as usize;
            let value = reader.bytes(size)?;
            reader.skip(size % 2).ok();

            match id {
                resource::ICC_PROFILE => resources.icc = Some(value),
                resource::VERSION_INFO => resources.has_merged_data = value.get(4) != Some(&0),
                resource::TRANSPARENCY_INDEX if value.len() >= 2 => {
                    resources.transparency_index = Some(u16::from_be_bytes([value[0], value[1]]));
                }
                resource::ALPHA_NAMES | resource::UNICODE_ALPHA_NAMES => {
                    resources.named_alpha_channels |= !value.is_empty();
                }
                _ => {}
            }
        }
        Ok(resources)
    }
}

/// The signed layer count, whose negative sign says the first extra channel
/// holds the merged transparency; None when the file has no layer info.
pub fn layer_count(section: &[u8], header: &Header) -> Result<Option<i16>, DecodeError> {
    let mut reader = ByteReader::new(section, Endian::Big);
    if reader.remaining() == 0 {
        return Ok(None);
    }
    let layer_info = header.read_length(&mut reader)?;
    if layer_info >= 2 {
        return Ok(Some(reader.u16()? as i16));
    }
    reader.skip(layer_info)?;
    let mask_info = reader.u32()? as usize;
    reader.skip(mask_info)?;

    // Deeper documents keep their layer info in a tagged block instead.
    for _ in 0..MAX_TAGGED_BLOCKS {
        if reader.remaining() < 12 || !matches!(reader.bytes(4)?, b"8BIM" | b"8B64") {
            break;
        }
        let key: &[u8; 4] = reader.bytes(4)?.try_into().expect("four bytes were read");
        let length = if header.is_psb() && LONG_LENGTH_KEYS.contains(&key) {
            header.read_length(&mut reader)?
        } else {
            reader.u32()? as usize
        };
        let data = reader.bytes(length)?;
        if LAYER_INFO_KEYS.contains(&key) && data.len() >= 2 {
            return Ok(Some(i16::from_be_bytes([data[0], data[1]])));
        }
        // Blocks are padded to four bytes.
        let padding = (4 - reader.position() % 4) % 4;
        reader.skip(padding.min(reader.remaining()))?;
    }
    Ok(None)
}
