//! Module: mirante::settings_dialog
//! Purpose: Settings: the display profile (monitor, preset or custom file) and
//! the plugin manager.
//! Created: 2026-11-16
//! Architecture: Plugins install or come off "just for me" directly, or "for
//! everyone" through pkexec and the mirante-plugin-admin helper; the viewer
//! itself never runs as root. Every change ends with `plugins::reload`.

use crate::{
    dialogs,
    display_profile::Choice,
    i18n::{self, tr},
    plugins, render,
};
use gtk::{pango, prelude::*};
use mirante_color::Preset;
use mirante_plugin::{ADMIN_HELPER, BUNDLE_EXTENSION, Installed, Roots, Scope, store};
use std::{cell::Cell, path::PathBuf, process::Command, rc::Rc};

const SPACING: i32 = 12;
const LIST_HEIGHT: i32 = 260;
const ICC_FOLDER: &str = "/usr/share/color/icc";
/// pkexec exit codes for "dismissed" and "not authorised": the user said no.
const PKEXEC_DECLINED: [i32; 2] = [126, 127];

struct Settings {
    dialog: gtk::Dialog,
    profiles: gtk::ComboBoxText,
    /// Set while the list is being refilled, so that is not taken as a choice.
    filling: Cell<bool>,
    list: gtk::ListBox,
    on_display_profile: Box<dyn Fn()>,
    on_plugins: Box<dyn Fn()>,
}

pub fn present(
    parent: &gtk::ApplicationWindow,
    on_display_profile: impl Fn() + 'static,
    on_plugins: impl Fn() + 'static,
) {
    let dialog = gtk::Dialog::builder().use_header_bar(1).transient_for(parent).build();
    dialog.set_title(tr("Settings"));
    dialog.set_default_size(520, -1);
    let content = gtk::Box::new(gtk::Orientation::Vertical, SPACING);
    content.set_border_width(SPACING as u32 * 2);

    let profile_row = gtk::Box::new(gtk::Orientation::Horizontal, SPACING);
    profile_row.pack_start(&heading("Display Profile"), false, false, 0);
    let profiles = gtk::ComboBoxText::new();
    profiles.set_hexpand(true);
    let load = gtk::Button::with_label("Load…");
    profile_row.pack_end(&load, false, false, 0);
    profile_row.pack_end(&profiles, true, true, 0);
    content.pack_start(&profile_row, false, false, 0);

    let plugins_row = gtk::Box::new(gtk::Orientation::Horizontal, SPACING);
    plugins_row.pack_start(&heading("Plugins"), false, false, 0);
    let install = gtk::Button::with_label("Install…");
    plugins_row.pack_end(&install, false, false, 0);
    content.pack_start(&plugins_row, false, false, 0);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    let scrolled = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
    scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scrolled.set_min_content_height(LIST_HEIGHT);
    scrolled.set_shadow_type(gtk::ShadowType::In);
    scrolled.add(&list);
    content.pack_start(&scrolled, true, true, 0);
    dialog.content_area().add(&content);

    let settings = Rc::new(Settings {
        dialog,
        profiles: profiles.clone(),
        filling: Cell::new(false),
        list,
        on_display_profile: Box::new(on_display_profile),
        on_plugins: Box::new(on_plugins),
    });
    let this = settings.clone();
    profiles.connect_changed(move |combo| {
        if !this.filling.get() {
            let choice = combo.active_id().and_then(|id| choice_from_id(&id));
            if let Some(choice) = choice {
                this.set_display_profile(choice);
            }
        }
    });
    let this = settings.clone();
    load.connect_clicked(move |_| this.choose_display_profile());
    let this = settings.clone();
    install.connect_clicked(move |_| this.choose_bundle());

    settings.fill_profiles();
    settings.fill_list();
    i18n::localize(&content);
    settings.dialog.show_all();
}

const MONITOR_ID: &str = "monitor";
const FILE_ID: &str = "file";
const PRESET_ID_PREFIX: &str = "preset:";

fn preset_id(preset: Preset) -> String {
    format!("{PRESET_ID_PREFIX}{}", preset.id())
}

/// The choice a list entry stands for; the custom file entry keeps the stored path.
fn choice_from_id(id: &str) -> Option<Choice> {
    match id {
        MONITOR_ID => Some(Choice::Monitor),
        FILE_ID => Some(Choice::stored()).filter(|choice| matches!(choice, Choice::File(_))),
        _ => id.strip_prefix(PRESET_ID_PREFIX).and_then(Preset::from_id).map(Choice::Preset),
    }
}

fn heading(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    let attributes = pango::AttrList::new();
    attributes.insert(pango::AttrInt::new_weight(pango::Weight::Bold));
    label.set_attributes(Some(&attributes));
    label
}

impl Settings {
    /// Lists the monitor, the presets and the custom file, if one was loaded.
    fn fill_profiles(&self) {
        self.filling.set(true);
        self.profiles.remove_all();
        self.profiles.append(Some(MONITOR_ID), tr("Current monitor"));
        for preset in Preset::ALL {
            self.profiles.append(Some(&preset_id(preset)), preset.name());
        }
        let choice = Choice::stored();
        if let Choice::File(path) = &choice {
            let name = path
                .file_name()
                .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
            self.profiles.append(Some(FILE_ID), &name);
        }
        let active = match choice {
            Choice::Monitor => MONITOR_ID.to_owned(),
            Choice::Preset(preset) => preset_id(preset),
            Choice::File(_) => FILE_ID.to_owned(),
        };
        self.profiles.set_active_id(Some(&active));
        self.filling.set(false);
    }

    fn set_display_profile(&self, choice: Choice) {
        if let Err(error) = choice.store() {
            eprintln!("mirante: could not save the display profile: {error}");
        }
        self.fill_profiles();
        (self.on_display_profile)();
    }

    fn choose_display_profile(self: &Rc<Self>) {
        let this = self.clone();
        let patterns = ["*.icc", "*.icm", "*.ICC", "*.ICM"].map(String::from);
        dialogs::choose_file(
            &self.dialog,
            "Display Profile",
            gtk::FileChooserAction::Open,
            "Open",
            |dialog| {
                dialog.add_filter(dialogs::filter("ICC profiles", &patterns, &[]));
                let _ = dialog.set_current_folder(ICC_FOLDER);
            },
            move |path| this.set_display_profile(Choice::File(path)),
        );
    }

    fn fill_list(self: &Rc<Self>) {
        self.list.children().iter().for_each(|row| self.list.remove(row));
        for plugin in plugins::installed() {
            self.list.add(&self.row(plugin));
        }
        self.list.show_all();
    }

    fn row(self: &Rc<Self>, plugin: Installed) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, SPACING);
        row.set_border_width(SPACING as u32 / 2);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let name = gtk::Label::new(Some(&plugin.manifest.name));
        name.set_xalign(0.0);
        let scope = if plugin.scope == Scope::User { tr("For me") } else { tr("For everyone") };
        let formats = plugin.manifest.extensions.join(", ");
        let detail =
            gtk::Label::new(Some(&format!("{} · {formats} · {scope}", plugin.manifest.version)));
        detail.set_xalign(0.0);
        detail.set_ellipsize(pango::EllipsizeMode::End);
        detail.style_context().add_class("dim-label");
        text.pack_start(&name, false, false, 0);
        text.pack_start(&detail, false, false, 0);
        row.pack_start(&text, true, true, 0);
        text.set_sensitive(plugin.enabled());

        let enabled = plugin.enabled();
        let button = gtk::Button::with_label(if enabled { tr("Remove") } else { tr("Restore") });
        button.set_valign(gtk::Align::Center);
        let this = self.clone();
        button.connect_clicked(
            move |_| if enabled { this.remove(&plugin) } else { this.restore(&plugin) },
        );
        row.pack_end(&button, false, false, 0);
        row
    }

    /// After any change: reload plugins, redraw the list, tell the window.
    fn changed(self: &Rc<Self>) {
        plugins::reload();
        self.fill_list();
        (self.on_plugins)();
    }

    fn report(self: &Rc<Self>, title: &str, result: Result<(), String>) {
        self.changed();
        if let Err(error) = result {
            dialogs::error(&self.dialog, title, &error);
        }
    }

    /// Runs the admin helper through pkexec off the main loop.
    fn as_admin(self: &Rc<Self>, args: Vec<String>, failure_title: &'static str) {
        let this = self.clone();
        render::in_background(
            move || Command::new("pkexec").arg(ADMIN_HELPER).args(args).output(),
            move |output| {
                let result = match output {
                    Ok(out) if out.status.success() => Ok(()),
                    Ok(out) if out.status.code().is_some_and(|c| PKEXEC_DECLINED.contains(&c)) => {
                        Ok(())
                    }
                    Ok(out) => Err(String::from_utf8_lossy(&out.stderr).trim().to_owned()),
                    Err(error) => Err(error.to_string()),
                };
                this.report(failure_title, result);
            },
        );
    }

    fn choose_bundle(self: &Rc<Self>) {
        let this = self.clone();
        let patterns = [format!("*.{BUNDLE_EXTENSION}")];
        dialogs::choose_file(
            &self.dialog,
            "Install…",
            gtk::FileChooserAction::Open,
            "Open",
            |dialog| dialog.add_filter(dialogs::filter("Mirante plugins", &patterns, &[])),
            move |path| this.install(path),
        );
    }

    fn install(self: &Rc<Self>, path: PathBuf) {
        const TITLE: &str = "Could not install the plugin";
        let manifest = std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| store::bundle_manifest(&bytes).map(|manifest| (manifest, bytes)));
        let (manifest, bytes) = match manifest {
            Ok(found) => found,
            Err(error) => return dialogs::error(&self.dialog, TITLE, &error),
        };
        let detail = format!("{} {}", manifest.name, manifest.version);
        let answer = dialogs::ask(
            &self.dialog,
            "Install this plugin?",
            &detail,
            &[
                ("Cancel", gtk::ResponseType::Cancel),
                ("Everyone", gtk::ResponseType::Other(1)),
                ("Just me", gtk::ResponseType::Accept),
            ],
        );
        match answer {
            gtk::ResponseType::Accept => {
                let result = store::install(&bytes, &Roots::from_env().user).map(drop);
                self.report(TITLE, result);
            }
            gtk::ResponseType::Other(1) => {
                self.as_admin(vec!["install".into(), path.to_string_lossy().into_owned()], TITLE)
            }
            _ => {}
        }
    }

    fn remove(self: &Rc<Self>, plugin: &Installed) {
        const TITLE: &str = "Could not remove the plugin";
        let id = plugin.manifest.id.clone();
        let mut buttons = vec![("Cancel", gtk::ResponseType::Cancel)];
        if plugin.scope == Scope::User {
            buttons.push(("Remove", gtk::ResponseType::Accept));
        } else {
            buttons.extend([
                ("Everyone", gtk::ResponseType::Other(1)),
                ("Just me", gtk::ResponseType::Accept),
            ]);
        }
        match dialogs::ask(&self.dialog, "Remove this plugin?", &plugin.manifest.name, &buttons) {
            gtk::ResponseType::Accept if plugin.scope == Scope::User => {
                self.report(TITLE, store::remove(&id, &Roots::from_env().user));
            }
            gtk::ResponseType::Accept => {
                self.report(
                    TITLE,
                    plugins::set_disabled_for_me(&id, true).map_err(|e| e.to_string()),
                );
            }
            gtk::ResponseType::Other(1) => self.as_admin(vec!["remove".into(), id], TITLE),
            _ => {}
        }
    }

    fn restore(self: &Rc<Self>, plugin: &Installed) {
        const TITLE: &str = "Could not install the plugin";
        let id = plugin.manifest.id.clone();
        if plugin.disabled_for_me {
            let result = plugins::set_disabled_for_me(&id, false).map_err(|e| e.to_string());
            if !plugin.disabled_for_everyone {
                return self.report(TITLE, result);
            }
        }
        if plugin.disabled_for_everyone {
            self.as_admin(vec!["restore".into(), id], TITLE);
        }
    }
}
