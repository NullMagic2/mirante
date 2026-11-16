//! Module: mirante_raw_fuji::compressed
//! Purpose: Decodes Fujifilm's lossless compressed RAF data (X-Trans and Bayer).
//! Created: 2026-11-16
//! Architecture: A 16-byte header and a table of block sizes precede the blocks;
//! each block is a 768-pixel-wide column strip coded independently (block.rs),
//! so blocks decode on separate threads and are stitched into one plane.
//! Header checks follow LibRaw's parse_fuji_compressed_header.

mod bits;
mod block;

use crate::header::Size;
use block::{Params, ROWS_PER_GROUP};
use mirante_codec::{ByteReader, Endian};
use mirante_core::DecodeError;
use std::thread;

const SIGNATURE: u16 = 0x4953;
const HEADER_LENGTH: usize = 16;
const BLOCK_WIDTH: usize = 0x300;
const XTRANS_TYPE: u8 = 16;
const BAYER_TYPE: u8 = 0;
const MAX_BLOCKS: usize = 16;
const MAX_WIDTH: usize = 0x4200;
const MIN_WIDTH: usize = 0x300;
const MAX_HEIGHT: usize = 0x4002;
/// Block sizes are stored in a table padded to this alignment.
const TABLE_ALIGN: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub lossless: bool,
    pub xtrans: bool,
    pub bits: u32,
    pub size: Size,
    blocks: usize,
    groups: usize,
}

fn invalid() -> DecodeError {
    DecodeError::corrupt("invalid Fujifilm compressed raw header")
}

impl Header {
    pub fn parse(data: &[u8]) -> Result<Self, DecodeError> {
        let mut reader = ByteReader::new(data, Endian::Big);
        let signature = reader.u16()?;
        let (lossless, raw_type, bits) = (reader.u8()?, reader.u8()?, reader.u8()?);
        let height = usize::from(reader.u16()?);
        let rounded_width = usize::from(reader.u16()?);
        let width = usize::from(reader.u16()?);
        let block_width = usize::from(reader.u16()?);
        let blocks = usize::from(reader.u8()?);
        let groups = usize::from(reader.u16()?);
        let valid = signature == SIGNATURE
            && lossless <= 1
            && matches!(raw_type, XTRANS_TYPE | BAYER_TYPE)
            && matches!(bits, 12 | 14 | 16)
            && block_width == BLOCK_WIDTH
            && (ROWS_PER_GROUP..=MAX_HEIGHT).contains(&height)
            && height % ROWS_PER_GROUP == 0
            && groups == height / ROWS_PER_GROUP
            && (MIN_WIDTH..=MAX_WIDTH).contains(&width)
            && width % 24 == 0
            && rounded_width <= MAX_WIDTH
            && rounded_width % block_width == 0
            && rounded_width >= width
            && rounded_width - width < block_width
            && (1..=MAX_BLOCKS).contains(&blocks)
            && blocks == rounded_width / block_width;
        if !valid {
            return Err(invalid());
        }
        Ok(Self {
            lossless: lossless == 1,
            xtrans: raw_type == XTRANS_TYPE,
            bits: u32::from(bits),
            size: Size { height, width },
            blocks,
            groups,
        })
    }
}

/// Splits the data after the header into each block's bytes.
fn blocks<'a>(data: &'a [u8], header: &Header) -> Result<Vec<&'a [u8]>, DecodeError> {
    let mut table = ByteReader::new(data, Endian::Big).at(HEADER_LENGTH)?;
    let mut start = HEADER_LENGTH + (4 * header.blocks).next_multiple_of(TABLE_ALIGN);
    (0..header.blocks)
        .map(|_| {
            let length = table.u32()? as usize;
            let bytes = data.get(start..).and_then(|rest| rest.get(..length));
            start += length;
            bytes.ok_or_else(|| DecodeError::corrupt("Fujifilm compressed block is truncated"))
        })
        .collect()
}

/// Every coded green sample costs at least one bit, so shorter blocks are truncated.
fn check_block_lengths(
    blocks: &[&[u8]],
    params: &Params,
    groups: usize,
) -> Result<(), DecodeError> {
    let minimum_bits = ROWS_PER_GROUP * params.line_width * groups;
    if blocks.iter().any(|block| (block.len() + 1) * 8 < minimum_bits) {
        return Err(DecodeError::corrupt("Fujifilm compressed block is truncated"));
    }
    Ok(())
}

/// Decodes the photosites; `colour(column, row)` is the CFA colour (0 R, 1 G, 2 B)
/// relative to the plane origin, which must repeat within 768 columns and 6 rows.
pub fn decode(
    data: &[u8],
    header: &Header,
    colour: impl Fn(usize, usize) -> u8,
) -> Result<Vec<u16>, DecodeError> {
    if !header.lossless {
        return Err(DecodeError::unsupported("lossy compressed Fujifilm RAW"));
    }
    let Size { height, width } = header.size;
    DecodeError::check_size(width as u32, height as u32)?;
    let blocks = blocks(data, header)?;
    let params = Params::new(header.bits, BLOCK_WIDTH, header.xtrans);
    check_block_lengths(&blocks, &params, header.groups)?;
    let sources = block::pixel_sources(&params, BLOCK_WIDTH, colour);

    let strips: Vec<Result<Vec<u16>, DecodeError>> = thread::scope(|scope| {
        let workers: Vec<_> = blocks
            .iter()
            .enumerate()
            .map(|(index, bytes)| {
                let strip_width = BLOCK_WIDTH.min(width - index * BLOCK_WIDTH);
                let (params, sources) = (&params, &sources);
                scope.spawn(move || {
                    block::decode_block(params, bytes, sources, strip_width, header.groups)
                })
            })
            .collect();
        workers.into_iter().map(|worker| worker.join().expect("block decoder panicked")).collect()
    });

    let mut plane = vec![0; width * height];
    for (index, strip) in strips.into_iter().enumerate() {
        let strip = strip?;
        let left = index * BLOCK_WIDTH;
        let strip_width = BLOCK_WIDTH.min(width - left);
        for (row, values) in plane.chunks_exact_mut(width).zip(strip.chunks_exact(strip_width)) {
            row[left..left + strip_width].copy_from_slice(values);
        }
    }
    Ok(plane)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A compressed-data header for `width` x `height`, X-Trans when `xtrans`.
    pub fn header_bytes(lossless: bool, xtrans: bool, width: u16, height: u16) -> Vec<u8> {
        let rounded = width.next_multiple_of(BLOCK_WIDTH as u16);
        let mut out = SIGNATURE.to_be_bytes().to_vec();
        out.extend([u8::from(lossless), if xtrans { XTRANS_TYPE } else { BAYER_TYPE }, 14]);
        for value in [height, rounded, width, BLOCK_WIDTH as u16] {
            out.extend(value.to_be_bytes());
        }
        out.push((rounded / BLOCK_WIDTH as u16) as u8);
        out.extend((height / ROWS_PER_GROUP as u16).to_be_bytes());
        out
    }

    #[test]
    fn parses_and_validates_headers() {
        let header = Header::parse(&header_bytes(true, true, 6048, 4038)).unwrap();
        assert_eq!(header.size, Size { height: 4038, width: 6048 });
        assert_eq!((header.lossless, header.xtrans, header.bits), (true, true, 14));
        assert_eq!((header.blocks, header.groups), (8, 673));
        let mut bad = header_bytes(true, true, 6048, 4038);
        bad[0] = b'X';
        assert!(Header::parse(&bad).is_err());
        let mut bad = header_bytes(true, true, 6048, 4038);
        bad[15] ^= 1;
        assert!(Header::parse(&bad).is_err());
        assert!(Header::parse(&header_bytes(true, true, 6050, 4038)).is_err());
        assert!(Header::parse(&header_bytes(true, true, 6048, 4037)).is_err());
    }

    #[test]
    fn splits_blocks_after_an_aligned_size_table() {
        let mut data = header_bytes(true, true, 2304, 6);
        for length in [2_u32, 3, 1] {
            data.extend(length.to_be_bytes());
        }
        data.extend([0; 4]);
        data.extend([1, 2, 3, 4, 5, 6]);
        let header = Header::parse(&data).unwrap();
        let found = blocks(&data, &header).unwrap();
        assert_eq!(found, [&[1, 2][..], &[3, 4, 5], &[6]]);
        assert!(blocks(&data[..data.len() - 1], &header).is_err());
    }

    #[test]
    fn rejects_lossy_and_short_blocks() {
        let lossy = header_bytes(false, false, 768, 6);
        let header = Header::parse(&lossy).unwrap();
        assert!(matches!(decode(&lossy, &header, |_, _| 1), Err(DecodeError::Unsupported(_))));
        let mut short = header_bytes(true, false, 768, 6);
        short.extend(4_u32.to_be_bytes());
        short.extend([0; 12 + 4]);
        let header = Header::parse(&short).unwrap();
        assert!(matches!(decode(&short, &header, |_, _| 1), Err(DecodeError::Corrupt(_))));
    }
}
