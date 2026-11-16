//! Module: mirante
//! Purpose: Starts the image viewer, or prints the MIME types it can open.
//! Created: 2026-11-16
//! Architecture: Format support comes from installed plugin executables; see
//! ARCHITECTURE.md. `--mime-types` lists what the installed plugins open.

mod adjust_panel;
mod crop_tool;
mod dialogs;
mod display_profile;
mod document;
mod edit_store;
mod i18n;
mod icons;
mod plugins;
mod render;
mod settings;
mod settings_dialog;
mod style;
mod theme;
mod toolbar;
mod translations;
mod viewer;
mod window;

use gtk::{gio, glib, prelude::*};

pub const APPLICATION_ID: &str = "io.github.mirante.Viewer";
/// A proper name, so it is never translated.
pub const APP_NAME: &str = "Mirante";
const MIME_TYPES_FLAG: &str = "--mime-types";

fn main() -> glib::ExitCode {
    if std::env::args().nth(1).as_deref() == Some(MIME_TYPES_FLAG) {
        println!("{}", plugins::registry().mime_types().join(";"));
        return glib::ExitCode::SUCCESS;
    }
    let application =
        gtk::Application::new(Some(APPLICATION_ID), gio::ApplicationFlags::HANDLES_OPEN);

    application.connect_startup(|_| {
        theme::apply_yaru();
        style::install();
        i18n::load();
    });
    application.connect_activate(|application| {
        window::Window::present_new(application, None);
    });
    application.connect_open(|application, files, _| {
        let path = files.first().and_then(|file| file.path());
        window::Window::present_new(application, path);
    });
    application.run()
}
