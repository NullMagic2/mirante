//! Module: mirante_format_tiff::jpeg
//! Purpose: Decodes JPEG-compressed (compression 7) strips and tiles.
//! Created: 2026-11-16
//! Architecture: Each chunk is an abbreviated JPEG stream; the shared JPEGTables
//! are spliced in and the result goes to the JPEG plugin's public decoder.

use crate::decode::Chunk;
use crate::layout::{Layout, photometric};
use mirante_core::{DecodeError, Rgba};

const SOI: [u8; 2] = [0xFF, 0xD8];
const EOI: [u8; 2] = [0xFF, 0xD9];
/// An Adobe APP14 segment with transform 0, telling the decoder that three
/// components are RGB rather than YCbCr.
const ADOBE_RGB: [u8; 16] =
    [0xFF, 0xEE, 0x00, 0x0E, b'A', b'd', b'o', b'b', b'e', 0x00, 0x64, 0, 0, 0, 0, 0];

pub fn decode_chunks(
    chunks: &[Chunk],
    tables: Option<&[u8]>,
    layout: &Layout,
) -> Result<Vec<Rgba>, DecodeError> {
    let width = layout.width as usize;
    let mut pixels = vec![[0.0, 0.0, 0.0, 1.0]; width * layout.height as usize];

    for chunk in chunks {
        let stream = splice(tables, chunk.data, layout.photometric == photometric::RGB);
        let tile = mirante_format_jpeg::decode(&stream)?;
        let visible_rows = chunk.rows.min(layout.height - chunk.y);
        let visible_width = chunk.width.min(layout.width - chunk.x);
        if tile.width() < visible_width || tile.height() < visible_rows {
            return Err(DecodeError::corrupt("TIFF JPEG tile is smaller than its area"));
        }
        for row in 0..visible_rows {
            let start = (chunk.y + row) as usize * width + chunk.x as usize;
            let source = &tile.row(row)[..visible_width as usize];
            pixels[start..start + source.len()].copy_from_slice(source);
        }
    }
    Ok(pixels)
}

/// Builds one complete stream: SOI, the optional RGB marker, the tables without
/// their own SOI/EOI, then the chunk without its SOI.
fn splice(tables: Option<&[u8]>, data: &[u8], rgb: bool) -> Vec<u8> {
    let tables = tables.unwrap_or_default();
    let tables = tables.strip_prefix(&SOI).unwrap_or(tables);
    let tables = tables.strip_suffix(&EOI).unwrap_or(tables);
    let body = data.strip_prefix(&SOI).unwrap_or(data);
    let mut stream = Vec::with_capacity(SOI.len() + ADOBE_RGB.len() + tables.len() + body.len());
    stream.extend_from_slice(&SOI);
    if rgb {
        stream.extend_from_slice(&ADOBE_RGB);
    }
    stream.extend_from_slice(tables);
    stream.extend_from_slice(body);
    stream
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splices_tables_into_abbreviated_stream() {
        let tables = [0xFF, 0xD8, 0xFF, 0xDB, 0xFF, 0xD9];
        let data = [0xFF, 0xD8, 0xFF, 0xDA, 0xFF, 0xD9];
        let stream = splice(Some(&tables), &data, false);
        assert_eq!(stream, [0xFF, 0xD8, 0xFF, 0xDB, 0xFF, 0xDA, 0xFF, 0xD9]);
    }
}
