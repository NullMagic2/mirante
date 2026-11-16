//! Module: mirante_codec::tar
//! Purpose: Reads and writes regular files in POSIX ustar archives.
//! Created: 2026-11-16
//! Architecture: Plugin bundles are plain ustar archives, so `tar` can build and
//! inspect them too. Links, devices and other entry types are skipped.

use mirante_core::DecodeError;

const BLOCK: usize = 512;
const NAME: std::ops::Range<usize> = 0..100;
const MODE: std::ops::Range<usize> = 100..108;
const SIZE: std::ops::Range<usize> = 124..136;
const CHECKSUM: std::ops::Range<usize> = 148..156;
const TYPE_FLAG: usize = 156;
const MAGIC: std::ops::Range<usize> = 257..262;
const PREFIX: std::ops::Range<usize> = 345..500;
const REGULAR_FILE: [u8; 2] = [b'0', 0];

pub struct Entry<'a> {
    pub name: String,
    pub mode: u32,
    pub data: &'a [u8],
}

fn text(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

fn octal(field: &[u8]) -> Result<usize, DecodeError> {
    let digits = text(field);
    usize::from_str_radix(digits.trim(), 8).map_err(|_| DecodeError::corrupt("bad tar number"))
}

/// Regular files in archive order; names keep their directory prefix.
pub fn entries(archive: &[u8]) -> Result<Vec<Entry<'_>>, DecodeError> {
    let mut entries = Vec::new();
    let mut offset = 0;

    while offset + BLOCK <= archive.len() {
        let header = &archive[offset..offset + BLOCK];
        if header.iter().all(|&b| b == 0) {
            break;
        }
        if &header[MAGIC] != b"ustar" {
            return Err(DecodeError::unsupported("only ustar archives are supported"));
        }
        let size = octal(&header[SIZE])?;
        let start = offset + BLOCK;
        let data = archive
            .get(start..start + size)
            .ok_or_else(|| DecodeError::corrupt("tar entry is truncated"))?;

        if REGULAR_FILE.contains(&header[TYPE_FLAG]) {
            let prefix = text(&header[PREFIX]);
            let name = text(&header[NAME]);
            let name = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
            entries.push(Entry { name, mode: octal(&header[MODE])? as u32, data });
        }
        offset = start + size.div_ceil(BLOCK) * BLOCK;
    }
    Ok(entries)
}

/// Writes a ustar archive of regular files: (name, mode, data).
pub fn write(files: &[(&str, u32, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, mode, data) in files {
        let mut header = [0_u8; BLOCK];
        let name = name.as_bytes();
        header[NAME][..name.len().min(NAME.len())]
            .copy_from_slice(&name[..name.len().min(NAME.len())]);
        header[MODE][..7].copy_from_slice(format!("{mode:07o}").as_bytes());
        header[SIZE][..11].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
        header[TYPE_FLAG] = REGULAR_FILE[0];
        header[MAGIC].copy_from_slice(b"ustar");
        header[MAGIC.end..MAGIC.end + 3].copy_from_slice(b"\x0000");
        // The checksum is computed with its own field read as spaces.
        header[CHECKSUM].fill(b' ');
        let sum: u32 = header.iter().map(|&b| u32::from(b)).sum();
        header[CHECKSUM][..7].copy_from_slice(format!("{sum:06o}\0").as_bytes());
        out.extend(header);
        out.extend(*data);
        out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
    }
    out.extend([0; BLOCK * 2]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_files_back() {
        let bytes = write(&[("a/plugin.ini", 0o644, b"id=x"), ("a/run", 0o755, &[1; 600])]);
        let entries = entries(&bytes).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(
            (entries[1].name.as_str(), entries[1].mode, entries[1].data.len()),
            ("a/run", 0o755, 600)
        );
    }

    #[test]
    fn truncated_archives_fail() {
        let bytes = write(&[("big", 0o644, &[0; 2000])]);
        assert!(entries(&bytes[..1024]).is_err());
    }
}
