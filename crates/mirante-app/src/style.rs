//! Module: mirante::style
//! Purpose: The visual touches Yaru does not give us: a dark canvas for photos,
//! a roomy bottom toolbar, and large rounded tool buttons tinted by group.
//! Created: 2026-11-16
//! Architecture: One application-wide CSS provider. Tints are mid-tone GNOME
//! palette colours, readable on light and dark Yaru alike; each tool group
//! gets one, used for its icons and a faint hover or checked background.

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
.mirante-tool:disabled {
    opacity: 0.35;
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

/// The colour of a group of tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tint {
    Blue,
    Purple,
    Orange,
    Teal,
    Red,
    Green,
    /// For the colourful triad icon, which keeps its own colours.
    Neutral,
}

impl Tint {
    const ALL: [Self; 7] =
        [Self::Blue, Self::Purple, Self::Orange, Self::Teal, Self::Red, Self::Green, Self::Neutral];

    pub fn class(self) -> &'static str {
        match self {
            Self::Blue => "tint-blue",
            Self::Purple => "tint-purple",
            Self::Orange => "tint-orange",
            Self::Teal => "tint-teal",
            Self::Red => "tint-red",
            Self::Green => "tint-green",
            Self::Neutral => "tint-neutral",
        }
    }

    fn colour(self) -> &'static str {
        match self {
            Self::Blue => "#3584e4",
            Self::Purple => "#9141ac",
            Self::Orange => "#e95420",
            Self::Teal => "#2190a4",
            Self::Red => "#e01b24",
            Self::Green => "#2ec27e",
            Self::Neutral => "#77767b",
        }
    }
}

/// Icon colour, hover and checked backgrounds for every tint.
fn tint_rules() -> String {
    Tint::ALL
        .iter()
        .map(|tint| {
            let (class, colour) = (tint.class(), tint.colour());
            format!(
                ".mirante-tool.{class} image {{ color: {colour}; }}\n\
                 .mirante-tool.{class}:hover {{ background-color: alpha({colour}, 0.14); }}\n\
                 .mirante-tool.{class}:checked {{ background-color: alpha({colour}, 0.24); }}\n"
            )
        })
        .collect()
}

pub fn install() {
    let Some(screen) = gdk::Screen::default() else { return };
    let provider = gtk::CssProvider::new();
    let css = format!("{CSS}{}", tint_rules());
    if let Err(error) = provider.load_from_data(css.as_bytes()) {
        eprintln!("mirante: built-in style rejected: {error}");
        return;
    }
    gtk::StyleContext::add_provider_for_screen(
        &screen,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
