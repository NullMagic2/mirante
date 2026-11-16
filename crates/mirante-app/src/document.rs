//! Module: mirante::document
//! Purpose: The open image, its edits, its folder, and loading and exporting it.
//! Created: 2026-11-16
//! Architecture: Pure Rust with no GTK, so it runs on worker threads. The source
//! image is converted to the working space once and shared read-only; edits live
//! in edit_store and are only baked into pixels by `export`.

use crate::edit_store;
use mirante_core::{Image, PluginRegistry};
use mirante_edit::Edits;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

pub struct Document {
    pub path: PathBuf,
    pub source: Arc<Image>,
    pub edits: Edits,
    /// The edit store's key for this file's content.
    pub key: String,
}

/// Reads and decodes a file, and finds the edits remembered for it.
pub fn load(registry: &PluginRegistry, path: &Path) -> Result<Document, String> {
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    let image = registry.decode(path, &bytes).map_err(|error| error.to_string())?;
    let key = edit_store::key(&bytes);
    let edits = edit_store::load(&key);
    let source = Arc::new(mirante_color::to_working(image));
    Ok(Document { path: path.to_path_buf(), source, edits, key })
}

/// Applies the edits at full resolution and writes the file `path` names.
pub fn export(
    registry: &PluginRegistry,
    source: &Image,
    edits: &Edits,
    path: &Path,
) -> Result<(), String> {
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
    let format = registry
        .writable_formats()
        .find(|format| format.has_extension(extension))
        .ok_or_else(|| format!("cannot write .{extension} files"))?;
    let encoder = registry.encoder(format.id).expect("writable formats have encoders");
    let image = mirante_color::to_export_srgb(edits.apply(source, None));
    let bytes = encoder.encode(&image).map_err(|error| error.to_string())?;
    fs::write(path, bytes).map_err(|error| error.to_string())
}

/// The next (step = 1) or previous (step = -1) openable file in the same folder.
pub fn neighbour(registry: &PluginRegistry, path: &Path, step: isize) -> Option<PathBuf> {
    let folder = path.parent()?;
    let mut files: Vec<PathBuf> = fs::read_dir(folder)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|candidate| candidate.is_file() && registry.can_open(candidate))
        .collect();
    files.sort_by_key(|file| file.file_name().map(|name| name.to_string_lossy().to_lowercase()));

    let index = files.iter().position(|file| file == path)? as isize;
    let next = (index + step).rem_euclid(files.len() as isize);
    (next != index).then(|| files[next as usize].clone())
}

/// A file name for exporting: the original stem plus "-edited", in a format we
/// can write, so the default never points at the original.
pub fn suggested_name(registry: &PluginRegistry, path: &Path) -> String {
    let stem = path.file_stem().map_or("image".into(), |stem| stem.to_string_lossy());
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
    let writable = registry.writable_formats().any(|format| format.has_extension(extension));
    let extension = if writable { extension } else { "png" };
    format!("{stem}-edited.{extension}")
}
