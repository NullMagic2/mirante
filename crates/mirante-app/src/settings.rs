//! Module: mirante::settings
//! Purpose: Per-user preferences stored as `key=value` lines.
//! Created: 2026-11-16
//! Architecture: Lives in $XDG_CONFIG_HOME/mirante/settings. Unknown keys are
//! kept so older and newer versions can share the file.

use gtk::glib;
use std::{collections::BTreeMap, fs, io, path::PathBuf};

pub const LANGUAGE: &str = "language";
pub const DISPLAY_PROFILE: &str = "display-profile";
pub const DISABLED_PLUGINS: &str = "disabled-plugins";

fn path() -> PathBuf {
    glib::user_config_dir().join("mirante").join("settings")
}

fn read_all() -> BTreeMap<String, String> {
    let text = fs::read_to_string(path()).unwrap_or_default();
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
        .collect()
}

pub fn get(key: &str) -> Option<String> {
    read_all().remove(key).filter(|value| !value.is_empty())
}

/// Stores a value; None removes the key.
pub fn set(key: &str, value: Option<&str>) -> io::Result<()> {
    let mut values = read_all();
    match value {
        Some(value) => values.insert(key.to_owned(), value.to_owned()),
        None => values.remove(key),
    };
    let path = path();
    fs::create_dir_all(path.parent().expect("settings path has a parent"))?;
    let text: String = values.iter().map(|(key, value)| format!("{key}={value}\n")).collect();
    fs::write(path, text)
}
