//! Module: mirante::theme
//! Purpose: Gives the window the Yaru look on Ubuntu, Debian and Mint alike.
//! Created: 2026-11-16
//! Architecture: Keeps the desktop's own Yaru variant (accent colour) when it
//! already uses one; otherwise picks Yaru or Yaru-dark to match the desktop's
//! light/dark preference. Missing themes leave GTK's default untouched.

use gtk::{gio, glib, prelude::*};
use std::path::PathBuf;

const THEME: &str = "Yaru";
const DARK_THEME: &str = "Yaru-dark";
const INTERFACE_SCHEMA: &str = "org.gnome.desktop.interface";
const COLOR_SCHEME_KEY: &str = "color-scheme";
const PREFER_DARK: &str = "prefer-dark";

pub fn apply_yaru() {
    let Some(settings) = gtk::Settings::default() else { return };
    let current = settings.gtk_theme_name().map(|name| name.to_string()).unwrap_or_default();

    if !current.starts_with(THEME) {
        let name = if prefers_dark(&current) { DARK_THEME } else { THEME };
        if installed("themes", name, "gtk-3.0/gtk.css") {
            settings.set_gtk_theme_name(Some(name));
        }
    }
    let icons = settings.gtk_icon_theme_name().map(|name| name.to_string()).unwrap_or_default();
    if !icons.starts_with(THEME) && installed("icons", THEME, "index.theme") {
        settings.set_gtk_icon_theme_name(Some(THEME));
    }
}

/// Mint and others name dark themes "…-Dark"; GNOME uses the color-scheme key.
fn prefers_dark(current_theme: &str) -> bool {
    if current_theme.to_lowercase().contains("dark") {
        return true;
    }
    let schema = gio::SettingsSchemaSource::default()
        .and_then(|source| source.lookup(INTERFACE_SCHEMA, true))
        .filter(|schema| schema.has_key(COLOR_SCHEME_KEY));
    schema.is_some_and(|_| {
        gio::Settings::new(INTERFACE_SCHEMA).string(COLOR_SCHEME_KEY) == PREFER_DARK
    })
}

fn installed(kind: &str, name: &str, marker: &str) -> bool {
    let mut roots: Vec<PathBuf> =
        vec![glib::user_data_dir(), glib::home_dir().join(format!(".{kind}"))];
    roots.extend(glib::system_data_dirs());
    roots.iter().any(|root| {
        let base = if root.ends_with(format!(".{kind}")) { root.clone() } else { root.join(kind) };
        base.join(name).join(marker).is_file()
    })
}
