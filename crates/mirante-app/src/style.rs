//! Module: mirante::style
//! Purpose: The few visual touches Yaru does not give us: a dark canvas for
//! photos, a roomy bottom toolbar and large, rounded tool buttons.
//! Created: 2026-11-16
//! Architecture: One application-wide CSS provider, using the theme's own
//! colours wherever possible so light and dark Yaru both look right.

use gtk::{gdk, prelude::*};

const CSS: &str = "
.mirante-canvas {
    background-color: #242424;
}
.mirante-canvas label {
    color: #f0f0f0;
}
.mirante-toolbar {
    padding: 8px 16px;
    background-color: @theme_bg_color;
    border-top: 1px solid @borders;
}
.mirante-tool {
    min-width: 40px;
    min-height: 40px;
    padding: 4px;
    border-radius: 10px;
}
.mirante-wide {
    min-width: 120px;
    min-height: 40px;
    border-radius: 10px;
}
.mirante-open {
    padding: 10px 24px;
    border-radius: 10px;
}
";

pub fn install() {
    let Some(screen) = gdk::Screen::default() else { return };
    let provider = gtk::CssProvider::new();
    if let Err(error) = provider.load_from_data(CSS.as_bytes()) {
        eprintln!("mirante: built-in style rejected: {error}");
        return;
    }
    gtk::StyleContext::add_provider_for_screen(
        &screen,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
