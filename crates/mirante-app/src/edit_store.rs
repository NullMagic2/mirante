//! Module: mirante::edit_store
//! Purpose: Remembers each image's edits without ever touching the image.
//! Created: 2026-11-16
//! Architecture: One small text file per image in $XDG_DATA_HOME/mirante/edits,
//! named by a hash of the file's size and first and last 64 KiB. The key
//! follows the content, so edits survive renaming or moving a photo.

use mirante_codec::checksum::fnv1a64;
use mirante_edit::Edits;
use mirante_plugin::store::data_home;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// Bytes hashed from each end of the file; enough to tell photos apart quickly.
const SAMPLE: usize = 64 * 1024;
const EXTENSION: &str = "edits";

/// The content key of a file's bytes.
pub fn key(bytes: &[u8]) -> String {
    let head = &bytes[..bytes.len().min(SAMPLE)];
    let tail = &bytes[bytes.len().saturating_sub(SAMPLE)..];
    let size = (bytes.len() as u64).to_le_bytes();
    format!("{:016x}", fnv1a64(&[&size, head, tail]))
}

fn path(key: &str) -> PathBuf {
    data_home().join("mirante").join("edits").join(format!("{key}.{EXTENSION}"))
}

pub fn load(key: &str) -> Edits {
    fs::read_to_string(path(key)).map(|text| Edits::from_text(&text)).unwrap_or_default()
}

/// Saves the edits; no edits means no file. `image` is noted for people reading it.
pub fn save(key: &str, edits: &Edits, image: &Path) -> io::Result<()> {
    let path = path(key);
    if edits.is_empty() {
        return fs::remove_file(&path).or_else(|error| {
            if error.kind() == io::ErrorKind::NotFound { Ok(()) } else { Err(error) }
        });
    }
    fs::create_dir_all(path.parent().expect("the store path has a parent"))?;
    let text = format!("# {}\n{}", image.display(), edits.to_text());
    fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_depends_on_content_and_size() {
        assert_ne!(key(b"abc"), key(b"abd"));
        assert_ne!(key(&[0; 10]), key(&[0; 11]));
        assert_eq!(key(b"same"), key(b"same"));
    }
}
