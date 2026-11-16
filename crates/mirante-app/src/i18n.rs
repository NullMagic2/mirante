//! Module: mirante::i18n
//! Purpose: The title-bar language menu and live re-translation of the window.
//! Created: 2026-11-16
//! Architecture: Same design as the QEMU Lightning and NTFS manager projects: a
//! flag menu in the header bar, a per-user choice, and property bindings that
//! re-translate visible text without restarting. Widgets are given English text;
//! `localize` registers them and the binding shows the current language.

use crate::{icons, settings, translations::CATALOG};
use gtk::{glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

const FLAG_WIDTH: i32 = 24;
const MENU_SPACING: i32 = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Pt,
    En,
    El,
    Es,
}

impl Language {
    pub const ALL: [Self; 4] = [Self::Pt, Self::En, Self::El, Self::Es];

    fn code(self) -> &'static str {
        match self {
            Self::Pt => "pt",
            Self::En => "en",
            Self::El => "el",
            Self::Es => "es",
        }
    }

    fn native_name(self) -> &'static str {
        match self {
            Self::Pt => "Português (Brasil)",
            Self::En => "English",
            Self::El => "Ελληνικά",
            Self::Es => "Español",
        }
    }

    fn flag(self) -> &'static [u8] {
        match self {
            Self::Pt => include_bytes!("../../../assets/flags/br-64.png"),
            Self::En => include_bytes!("../../../assets/flags/us-64.png"),
            Self::El => include_bytes!("../../../assets/flags/gr-64.png"),
            Self::Es => include_bytes!("../../../assets/flags/es-64.png"),
        }
    }

    /// Column in the catalog; None for the English source text.
    fn column(self) -> Option<usize> {
        match self {
            Self::En => None,
            Self::Pt => Some(0),
            Self::El => Some(1),
            Self::Es => Some(2),
        }
    }

    fn parse(tag: &str) -> Option<Self> {
        let code = tag.split(['_', '-', '.', '@']).next().unwrap_or_default();
        Self::ALL.into_iter().find(|language| language.code() == code)
    }
}

thread_local! {
    static CURRENT: Cell<Language> = const { Cell::new(Language::En) };
    static BINDINGS: RefCell<Vec<Rc<Binding>>> = const { RefCell::new(Vec::new()) };
}

/// Saved choice first, then the same variables gettext reads, then English.
pub fn load() {
    let saved = settings::get(settings::LANGUAGE).and_then(|code| Language::parse(&code));
    let system = ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"].into_iter().find_map(|key| {
        let value = std::env::var(key).ok()?;
        value.split(':').find_map(Language::parse)
    });
    CURRENT.with(|current| current.set(saved.or(system).unwrap_or(Language::En)));
}

pub fn current() -> Language {
    CURRENT.with(Cell::get)
}

/// Translates English source text; unknown text is returned unchanged.
pub fn tr(text: &str) -> &str {
    let Some(column) = current().column() else { return text };
    CATALOG.iter().find(|(source, _)| *source == text).map_or(text, |(_, row)| row[column])
}

fn choose(language: Language) {
    if let Err(error) = settings::set(settings::LANGUAGE, Some(language.code())) {
        eprintln!("mirante: could not save the language: {error}");
    }
    CURRENT.with(|current| current.set(language));
    let bindings = BINDINGS.with(|list| list.borrow().clone());
    bindings.iter().for_each(|binding| binding.apply());
}

/// Remembers a property's English text and keeps it translated.
struct Binding {
    object: glib::WeakRef<glib::Object>,
    property: &'static str,
    source: RefCell<String>,
    updating: Cell<bool>,
}

impl Binding {
    fn apply(&self) {
        let Some(object) = self.object.upgrade() else { return };
        self.updating.set(true);
        let translated = tr(&self.source.borrow()).to_owned();
        if object.property::<Option<String>>(self.property).as_deref() != Some(&translated) {
            object.set_property(self.property, translated);
        }
        self.updating.set(false);
    }
}

fn bind(object: &impl IsA<glib::Object>, property: &'static str) {
    let object = object.as_ref();
    let known = BINDINGS.with(|list| {
        list.borrow()
            .iter()
            .any(|b| b.property == property && b.object.upgrade().as_ref() == Some(object))
    });
    let Some(source) = object.property::<Option<String>>(property) else { return };
    if known {
        return;
    }
    let binding = Rc::new(Binding {
        object: object.downgrade(),
        property,
        source: RefCell::new(source),
        updating: Cell::new(false),
    });
    let watcher = binding.clone();
    // Code that sets new English text later is picked up and translated too.
    object.connect_notify_local(Some(property), move |object, _| {
        if watcher.updating.get() {
            return;
        }
        *watcher.source.borrow_mut() =
            object.property::<Option<String>>(property).unwrap_or_default();
        watcher.apply();
    });
    binding.apply();
    BINDINGS.with(|list| list.borrow_mut().push(binding));
}

/// Registers the visible text of a widget tree. File names are never passed here.
pub fn localize(widget: &impl IsA<gtk::Widget>) {
    fn visit(widget: &gtk::Widget) {
        bind(widget, "tooltip-text");
        if widget.is::<gtk::Label>() || widget.is::<gtk::Button>() || widget.is::<gtk::MenuItem>() {
            bind(widget, "label");
        }
        if widget.is::<gtk::MessageDialog>() {
            bind(widget, "text");
        }
        if let Some(container) = widget.downcast_ref::<gtk::Container>() {
            container.children().iter().for_each(visit);
        }
    }
    BINDINGS.with(|list| list.borrow_mut().retain(|b| b.object.upgrade().is_some()));
    visit(widget.as_ref());
}

/// The header-bar button: current flag, popover with flags and native names.
pub fn selector(on_change: impl Fn() + 'static) -> gtk::MenuButton {
    let button = gtk::MenuButton::new();
    button.add(&icons::png_image(current().flag(), FLAG_WIDTH));
    button.set_tooltip_text(Some("Language"));

    let popover = gtk::Popover::new(Some(&button));
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list.set_border_width(MENU_SPACING as u32 / 2);
    let on_change = Rc::new(on_change);
    let ticks = Rc::new(RefCell::new(Vec::<(Language, gtk::Image)>::new()));

    for language in Language::ALL {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, MENU_SPACING);
        row.pack_start(&icons::png_image(language.flag(), FLAG_WIDTH), false, false, 0);
        row.pack_start(&gtk::Label::new(Some(language.native_name())), true, true, 0);
        let tick = gtk::Image::from_icon_name(Some("object-select-symbolic"), gtk::IconSize::Menu);
        tick.set_opacity(if language == current() { 1.0 } else { 0.0 });
        row.pack_end(&tick, false, false, 0);
        ticks.borrow_mut().push((language, tick));

        let item = gtk::Button::new();
        item.set_relief(gtk::ReliefStyle::None);
        item.add(&row);
        let (button, ticks, on_change) = (button.downgrade(), ticks.clone(), on_change.clone());
        item.connect_clicked(move |_| {
            let Some(button) = button.upgrade() else { return };
            choose(language);
            if let Some(child) = button.child() {
                button.remove(&child);
            }
            button.add(&icons::png_image(language.flag(), FLAG_WIDTH));
            button.show_all();
            for (value, tick) in ticks.borrow().iter() {
                tick.set_opacity(if *value == language { 1.0 } else { 0.0 });
            }
            on_change();
        });
        list.pack_start(&item, false, false, 0);
    }
    popover.add(&list);
    list.show_all();
    button.set_popover(Some(&popover));
    localize(&button);
    button
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_tags_resolve() {
        assert!(matches!(Language::parse("pt_BR.UTF-8"), Some(Language::Pt)));
        assert!(matches!(Language::parse("el_GR"), Some(Language::El)));
        assert!(Language::parse("de_DE").is_none());
    }

    #[test]
    fn catalog_has_no_duplicates_or_blanks() {
        for (index, (source, row)) in CATALOG.iter().enumerate() {
            assert!(row.iter().all(|text| !text.is_empty()), "{source}");
            assert!(CATALOG[..index].iter().all(|(other, _)| other != source), "{source}");
        }
    }
}
