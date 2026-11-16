//! Module: mirante::dialogs
//! Purpose: The few dialogs every part of the window shares: file choosers,
//! questions and errors.
//! Created: 2026-11-16
//! Architecture: Every dialog is modal and runs to completion, so callers get
//! their answer back directly and nothing has to be kept alive afterwards.

use crate::i18n::{self, tr};
use gtk::prelude::*;
use std::path::PathBuf;

/// A named filter of glob patterns and MIME types.
pub fn filter(name: &str, patterns: &[String], mime_types: &[&str]) -> gtk::FileFilter {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(tr(name)));
    patterns.iter().for_each(|pattern| filter.add_pattern(pattern));
    mime_types.iter().for_each(|mime| filter.add_mime_type(mime));
    filter
}

/// Shows a modal file chooser; `chosen` runs with the picked path. GTK's own
/// dialog is used because the native one ignores clicks over a modal parent.
pub fn choose_file(
    parent: &impl IsA<gtk::Window>,
    title: &str,
    action: gtk::FileChooserAction,
    accept: &str,
    setup: impl FnOnce(&gtk::FileChooserDialog),
    chosen: impl FnOnce(PathBuf),
) {
    let dialog = gtk::FileChooserDialog::with_buttons(
        Some(tr(title)),
        Some(parent),
        action,
        &[(tr("Cancel"), gtk::ResponseType::Cancel), (tr(accept), gtk::ResponseType::Accept)],
    );
    dialog.set_default_response(gtk::ResponseType::Accept);
    if let Some(button) = dialog.widget_for_response(gtk::ResponseType::Accept) {
        button.style_context().add_class("suggested-action");
    }
    setup(&dialog);
    let response = dialog.run();
    let path = dialog.filename();
    dialog.close();
    if let (gtk::ResponseType::Accept, Some(path)) = (response, path) {
        chosen(path);
    }
}

/// Asks a question with the given (label, response) buttons; the last is the default.
pub fn ask(
    parent: &impl IsA<gtk::Window>,
    question: &str,
    detail: &str,
    buttons: &[(&str, gtk::ResponseType)],
) -> gtk::ResponseType {
    let dialog = gtk::MessageDialog::new(
        Some(parent),
        gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
        gtk::MessageType::Question,
        gtk::ButtonsType::None,
        question,
    );
    if !detail.is_empty() {
        dialog.set_secondary_text(Some(detail));
    }
    for (label, response) in buttons {
        dialog.add_button(label, *response);
    }
    if let Some((_, response)) = buttons.last() {
        dialog.set_default_response(*response);
    }
    i18n::localize(&dialog);
    let answer = dialog.run();
    dialog.close();
    answer
}

pub fn error(parent: &impl IsA<gtk::Window>, title: &str, detail: &str) {
    let dialog = gtk::MessageDialog::new(
        Some(parent),
        gtk::DialogFlags::MODAL | gtk::DialogFlags::DESTROY_WITH_PARENT,
        gtk::MessageType::Error,
        gtk::ButtonsType::Close,
        title,
    );
    dialog.set_secondary_text(Some(detail));
    i18n::localize(&dialog);
    dialog.run();
    dialog.close();
}
