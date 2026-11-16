//! Module: mirante::window
//! Purpose: The viewer window: header bar, actions, and the open/edit/save flow.
//! Created: 2026-11-16
//! Architecture: Owns the Document and decides when to render; Viewer draws,
//! AdjustPanel edits colour, document.rs does file work on worker threads.
//! Header, left to right: Open, Previous/Next … Crop, Adjust, Language, Menu.

use crate::{
    adjust_panel::AdjustPanel,
    dialogs, display_profile,
    document::{self, Document},
    edit_store,
    i18n::{self, tr},
    plugins,
    render::{self, Job, Renderer},
    settings_dialog,
    viewer::Viewer,
};
use gtk::{gdk, gio, glib, prelude::*};
use mirante_color::{DisplayTransform, Profile};
use mirante_edit::{Edits, Orientation};
use std::{
    cell::{Cell, OnceCell, RefCell},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
};

const DEFAULT_SIZE: (i32, i32) = (1000, 700);
const SAVE_DELAY: std::time::Duration = std::time::Duration::from_millis(400);
const ZOOM_STEP: f64 = 1.25;
const EMPTY_PAGE: &str = "empty";
const IMAGE_PAGE: &str = "image";

/// Identifies a render so identical requests are skipped (lazy rendering).
type RenderKey = (usize, Edits, (u32, u32), usize);

fn icon_button(icon: &str, tooltip: &str, action: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(Some(icon), gtk::IconSize::Button);
    button.set_tooltip_text(Some(tooltip));
    button.set_action_name(Some(action));
    button
}

pub struct Window {
    window: gtk::ApplicationWindow,
    header: gtk::HeaderBar,
    stack: gtk::Stack,
    viewer: Rc<Viewer>,
    crop_button: gtk::ToggleButton,
    /// Apply / Cancel, shown only while cropping.
    crop_bar: gtk::ActionBar,
    adjust_button: gtk::MenuButton,
    menu_button: gtk::MenuButton,
    adjust: OnceCell<Rc<AdjustPanel>>,
    renderer: OnceCell<Rc<Renderer>>,
    document: RefCell<Option<Document>>,
    display: RefCell<Arc<DisplayTransform>>,
    monitor: RefCell<Option<String>>,
    last_render: RefCell<Option<RenderKey>>,
    /// Set just before leaving crop mode when the selection must be discarded.
    discard_crop: Cell<bool>,
    /// Pending write of the edits; slider drags are saved once they settle.
    save_timer: RefCell<Option<glib::SourceId>>,
}

impl Window {
    pub fn present_new(application: &gtk::Application, path: Option<PathBuf>) {
        let window = Self::build(application);
        // The GTK window owns this struct: keep it alive until the window is destroyed.
        let keeper = RefCell::new(Some(window.clone()));
        window.window.connect_destroy(move |_| drop(keeper.take()));
        window.window.show_all();
        window.reload_display_profile();
        if let Some(path) = path {
            window.open(path);
        }
    }

    fn build(application: &gtk::Application) -> Rc<Self> {
        let window = gtk::ApplicationWindow::new(application);
        window.set_default_size(DEFAULT_SIZE.0, DEFAULT_SIZE.1);
        window.set_icon_name(Some(crate::APPLICATION_ID));

        let header = gtk::HeaderBar::new();
        header.set_show_close_button(true);
        window.set_titlebar(Some(&header));

        let viewer = Viewer::new();
        let stack = gtk::Stack::new();
        let open = gtk::Button::with_label("Open…");
        open.set_image(Some(&gtk::Image::from_icon_name(
            Some("document-open-symbolic"),
            gtk::IconSize::Button,
        )));
        open.set_always_show_image(true);
        open.set_action_name(Some("win.open"));
        open.set_halign(gtk::Align::Center);
        open.set_valign(gtk::Align::Center);
        stack.add_named(&open, EMPTY_PAGE);
        stack.add_named(&viewer.scrolled, IMAGE_PAGE);
        let crop_bar = crop_bar();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.pack_start(&stack, true, true, 0);
        content.pack_end(&crop_bar, false, false, 0);
        window.add(&content);

        let crop_button = gtk::ToggleButton::new();
        crop_button.set_image(Some(&gtk::Image::from_icon_name(
            Some("image-crop-symbolic"),
            gtk::IconSize::Button,
        )));
        crop_button.set_tooltip_text(Some("Crop"));
        let adjust_button = gtk::MenuButton::new();
        adjust_button.set_image(Some(&gtk::Image::from_icon_name(
            Some("preferences-color-symbolic"),
            gtk::IconSize::Button,
        )));
        adjust_button.set_tooltip_text(Some("Adjust"));
        let menu_button = gtk::MenuButton::new();
        menu_button.set_image(Some(&gtk::Image::from_icon_name(
            Some("open-menu-symbolic"),
            gtk::IconSize::Button,
        )));
        menu_button.set_tooltip_text(Some("Menu"));

        let navigation = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        navigation.style_context().add_class("linked");
        navigation.add(&icon_button("go-previous-symbolic", "Previous", "win.previous"));
        navigation.add(&icon_button("go-next-symbolic", "Next", "win.next"));
        header.pack_start(&icon_button("document-open-symbolic", "Open", "win.open"));
        header.pack_start(&navigation);

        let this = Rc::new_cyclic(|weak: &std::rc::Weak<Self>| {
            let renderer = Renderer::new({
                let weak = weak.clone();
                move |frame| {
                    if let Some(this) = weak.upgrade() {
                        this.viewer.show_frame(frame);
                    }
                }
            });
            let adjust = AdjustPanel::new(&adjust_button, {
                let weak = weak.clone();
                move |adjustments| {
                    if let Some(this) = weak.upgrade() {
                        this.update_edits(|edits| edits.adjustments = adjustments);
                    }
                }
            });
            adjust_button.set_popover(Some(&adjust.popover));
            Self {
                window,
                header,
                stack,
                viewer,
                crop_button,
                crop_bar,
                adjust_button,
                menu_button,
                adjust: OnceCell::from(adjust),
                renderer: OnceCell::from(renderer),
                document: RefCell::new(None),
                display: RefCell::new(Arc::new(DisplayTransform::new(&Profile::srgb()))),
                monitor: RefCell::new(None),
                last_render: RefCell::new(None),
                discard_crop: Cell::new(false),
                save_timer: RefCell::new(None),
            }
        });
        let weak = Rc::downgrade(&this);
        let language = i18n::selector(move || {
            if let Some(this) = weak.upgrade() {
                this.menu_button.set_menu_model(Some(&build_menu()));
                this.update_title();
            }
        });
        this.header.pack_end(&this.menu_button);
        this.header.pack_end(&language);
        this.header.pack_end(&this.adjust_button);
        this.header.pack_end(&this.crop_button);
        this.header.pack_end(&transform_buttons());
        this.menu_button.set_menu_model(Some(&build_menu()));

        this.install_actions(application);
        this.connect_signals();
        i18n::localize(&this.window);
        this.update_title();
        this.update_sensitivity();
        this
    }

    fn install_actions(self: &Rc<Self>, application: &gtk::Application) {
        let add = |name: &str, accels: &[&str], run: fn(&Rc<Self>)| {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(this) = weak.upgrade() {
                    run(&this);
                }
            });
            self.window.add_action(&action);
            application.set_accels_for_action(&format!("win.{name}"), accels);
        };
        add("open", &["<Control>o"], |this| this.choose_file());
        add("export", &["<Control>e", "<Control><Shift>s"], |this| this.choose_export_path());
        add("revert", &[], |this| {
            this.update_edits(|edits| *edits = Edits::default());
            this.sync_adjust_panel();
        });
        add("previous", &[], |this| this.navigate(-1));
        add("next", &[], |this| this.navigate(1));
        add("settings", &["<Control>comma"], |this| this.open_settings());
        add("crop-apply", &[], |this| this.crop_button.set_active(false));
        add("rotate-left", &["<Control><Shift>r"], |this| {
            this.reorient(Orientation::rotated_counter_clockwise)
        });
        add("rotate-right", &["<Control>r"], |this| this.reorient(Orientation::rotated_clockwise));
        add("flip-horizontal", &[], |this| this.reorient(Orientation::flipped_horizontally));
        add("flip-vertical", &[], |this| this.reorient(Orientation::flipped_vertically));
        add("crop-cancel", &[], |this| this.cancel_crop());
        add("zoom-in", &["<Control>plus", "<Control>equal", "<Control>KP_Add"], |this| {
            this.viewer.zoom_by(ZOOM_STEP)
        });
        add("zoom-out", &["<Control>minus", "<Control>KP_Subtract"], |this| {
            this.viewer.zoom_by(1.0 / ZOOM_STEP)
        });
        add("zoom-fit", &["<Control>0", "<Control>KP_0"], |this| this.viewer.set_zoom(None));
        add("zoom-original", &["<Control>1", "<Control>KP_1"], |this| {
            this.viewer.set_zoom(Some(1.0))
        });
    }

    fn connect_signals(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.viewer.connect_resize(move || {
            if let Some(this) = weak.upgrade() {
                this.refresh();
            }
        });
        let weak = Rc::downgrade(self);
        self.crop_button.connect_toggled(move |button| {
            if let Some(this) = weak.upgrade() {
                this.crop_toggled(button.is_active());
            }
        });
        let weak = Rc::downgrade(self);
        self.window.connect_key_press_event(move |_, event| match weak.upgrade() {
            Some(this) if this.handle_key(event.keyval()) => glib::Propagation::Stop,
            _ => glib::Propagation::Proceed,
        });
        let weak = Rc::downgrade(self);
        self.window.connect_configure_event(move |window, _| {
            if let Some(this) = weak.upgrade() {
                let model = display_profile::monitor_model(window.upcast_ref());
                if *this.monitor.borrow() != model {
                    this.reload_display_profile();
                }
            }
            false
        });
        let weak = Rc::downgrade(self);
        self.window.connect_delete_event(move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.flush_edits();
            }
            glib::Propagation::Proceed
        });

        let targets = [gtk::TargetEntry::new("text/uri-list", gtk::TargetFlags::OTHER_APP, 0)];
        self.window.drag_dest_set(gtk::DestDefaults::ALL, &targets, gdk::DragAction::COPY);
        let weak = Rc::downgrade(self);
        self.window.connect_drag_data_received(move |_, _, _, _, data, _, _| {
            let path = data.uris().first().and_then(|uri| gio::File::for_uri(uri).path());
            if let (Some(this), Some(path)) = (weak.upgrade(), path) {
                this.open(path);
            }
        });
    }

    /// Plain keys; skipped while the adjust popover is open so sliders get arrows.
    fn handle_key(self: &Rc<Self>, key: gdk::keys::Key) -> bool {
        use gdk::keys::constants as keys;
        let cropping = self.crop_button.is_active();
        if self.adjust_button.is_active() || self.document.borrow().is_none() {
            return false;
        }
        match key {
            keys::Escape if cropping => self.cancel_crop(),
            keys::Return | keys::KP_Enter if cropping => self.crop_button.set_active(false),
            keys::Escape => self.window.unfullscreen(),
            keys::F11 => self.toggle_fullscreen(),
            keys::Left | keys::Page_Up if !cropping => self.navigate(-1),
            keys::Right | keys::Page_Down if !cropping => self.navigate(1),
            keys::plus | keys::equal | keys::KP_Add => self.viewer.zoom_by(ZOOM_STEP),
            keys::minus | keys::KP_Subtract => self.viewer.zoom_by(1.0 / ZOOM_STEP),
            keys::_0 | keys::KP_0 => self.viewer.set_zoom(None),
            keys::_1 | keys::KP_1 => self.viewer.set_zoom(Some(1.0)),
            _ => return false,
        }
        true
    }

    fn toggle_fullscreen(&self) {
        let fullscreen =
            self.window.window().is_some_and(|w| w.state().contains(gdk::WindowState::FULLSCREEN));
        if fullscreen { self.window.unfullscreen() } else { self.window.fullscreen() }
    }

    fn update_title(&self) {
        let document = self.document.borrow();
        let name = document
            .as_ref()
            .and_then(|d| d.path.file_name())
            .map(|n| n.to_string_lossy().into_owned());
        let title = name.unwrap_or_else(|| crate::APP_NAME.to_owned());
        self.header.set_title(Some(&title));
        self.window.set_title(&title);
    }

    fn update_sensitivity(&self) {
        let document = self.document.borrow();
        let loaded = document.is_some();
        let edited = document.as_ref().is_some_and(|d| !d.edits.is_empty());
        for name in [
            "export",
            "previous",
            "next",
            "zoom-in",
            "zoom-out",
            "zoom-fit",
            "zoom-original",
            "rotate-left",
            "rotate-right",
            "flip-horizontal",
            "flip-vertical",
        ] {
            self.set_action_enabled(name, loaded);
        }
        self.set_action_enabled("revert", edited);
        self.crop_button.set_sensitive(loaded);
        self.adjust_button.set_sensitive(loaded);
        self.stack.set_visible_child_name(if loaded { IMAGE_PAGE } else { EMPTY_PAGE });
    }

    fn set_action_enabled(&self, name: &str, enabled: bool) {
        let action =
            self.window.lookup_action(name).and_then(|a| a.downcast::<gio::SimpleAction>().ok());
        if let Some(action) = action {
            action.set_enabled(enabled);
        }
    }

    fn update_edits(self: &Rc<Self>, change: impl FnOnce(&mut Edits)) {
        if let Some(document) = self.document.borrow_mut().as_mut() {
            change(&mut document.edits);
        }
        self.update_sensitivity();
        self.refresh();
        self.schedule_save();
    }

    /// Writes the edits shortly after the last change, so a slider drag is one write.
    fn schedule_save(self: &Rc<Self>) {
        if let Some(pending) = self.save_timer.take() {
            pending.remove();
        }
        let weak = Rc::downgrade(self);
        let timer = glib::timeout_add_local_once(SAVE_DELAY, move || {
            if let Some(this) = weak.upgrade() {
                // The source is gone once it fires; forget it without removing it.
                this.save_timer.take();
                this.save_edits();
            }
        });
        self.save_timer.replace(Some(timer));
    }

    /// Saves now if a save is pending, as before switching images or closing.
    fn flush_edits(&self) {
        if let Some(pending) = self.save_timer.take() {
            pending.remove();
            self.save_edits();
        }
    }

    fn save_edits(&self) {
        if let Some(document) = self.document.borrow().as_ref()
            && let Err(error) = edit_store::save(&document.key, &document.edits, &document.path)
        {
            eprintln!("mirante: could not remember the edits: {error}");
        }
    }

    fn sync_adjust_panel(&self) {
        let adjustments =
            self.document.borrow().as_ref().map(|d| d.edits.adjustments).unwrap_or_default();
        self.adjust.get().expect("built in Window::build").set(adjustments);
    }

    /// Starts a render if what should be on screen changed since the last one.
    fn refresh(&self) {
        let document = self.document.borrow();
        let Some(document) = document.as_ref() else {
            self.viewer.set_image_size(None);
            return;
        };
        let mut edits = document.edits;
        if self.crop_button.is_active() {
            edits.crop = None;
        }
        let size = (document.source.width(), document.source.height());
        self.viewer.set_image_size(Some(edits.output_size(size)));

        let Some(max_size) = self.viewer.render_size() else { return };
        let display = self.display.borrow().clone();
        let key = (
            Arc::as_ptr(&document.source) as usize,
            edits,
            max_size,
            Arc::as_ptr(&display) as usize,
        );
        if self.last_render.replace(Some(key)) == Some(key) {
            return;
        }
        let job = Job { source: document.source.clone(), edits, max_size, display };
        self.renderer.get().expect("built in Window::build").request(job);
    }

    /// Rotates or flips the picture; the crop follows the content. Not while cropping.
    fn reorient(self: &Rc<Self>, change: fn(Orientation) -> Orientation) {
        if self.crop_button.is_active() {
            return;
        }
        let size = self.document.borrow().as_ref().map(|d| (d.source.width(), d.source.height()));
        if let Some(size) = size {
            self.viewer.set_zoom(None);
            self.update_edits(|edits| edits.reorient(change, size));
        }
    }

    /// Leaves crop mode without changing the crop.
    fn cancel_crop(&self) {
        if self.crop_button.is_active() {
            self.discard_crop.set(true);
            self.crop_button.set_active(false);
        }
    }

    fn crop_toggled(self: &Rc<Self>, active: bool) {
        self.crop_bar.set_visible(active);
        if active {
            let Some((crop, bounds)) = self.document.borrow().as_ref().map(|d| {
                let size = (d.source.width(), d.source.height());
                (d.edits.crop, d.edits.orientation.displayed_size(size))
            }) else {
                return;
            };
            self.viewer.begin_crop(crop, bounds);
            self.refresh();
            return;
        }
        let crop = self.viewer.end_crop();
        if self.discard_crop.replace(false) {
            self.refresh();
        } else {
            self.update_edits(|edits| edits.crop = crop);
        }
    }

    pub fn open(self: &Rc<Self>, path: PathBuf) {
        let registry = plugins::registry();
        let weak = Rc::downgrade(self);
        let target = path.clone();
        render::in_background(
            move || document::load(&registry, &target),
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                match result {
                    Ok(document) => this.set_document(document),
                    Err(error) => dialogs::error(&this.window, "Could not open the image", &error),
                }
            },
        );
    }

    fn set_document(&self, document: Document) {
        self.flush_edits();
        if self.crop_button.is_active() {
            self.discard_crop.set(true);
            self.crop_button.set_active(false);
        }
        self.document.replace(Some(document));
        self.sync_adjust_panel();
        self.viewer.set_image_size(None);
        self.viewer.set_zoom(None);
        self.update_title();
        self.update_sensitivity();
        self.refresh();
    }

    fn navigate(self: &Rc<Self>, step: isize) {
        let path = self.document.borrow().as_ref().map(|d| d.path.clone());
        if let Some(next) =
            path.and_then(|path| document::neighbour(&plugins::registry(), &path, step))
        {
            self.open(next);
        }
    }

    fn folder(&self) -> Option<PathBuf> {
        self.document.borrow().as_ref().and_then(|d| d.path.parent().map(PathBuf::from))
    }

    fn choose_file(self: &Rc<Self>) {
        let registry = plugins::registry();
        let mut patterns = Vec::new();
        let mut mime_types = Vec::new();
        for format in registry.readable_formats() {
            mime_types.extend(format.mime_types);
            for extension in format.extensions {
                patterns
                    .extend([format!("*.{extension}"), format!("*.{}", extension.to_uppercase())]);
            }
        }
        let folder = self.folder();
        let weak = Rc::downgrade(self);
        dialogs::choose_file(
            &self.window,
            "Open Image",
            gtk::FileChooserAction::Open,
            "Open",
            |dialog| {
                dialog.add_filter(dialogs::filter("All images", &patterns, &mime_types));
                if let Some(folder) = folder {
                    let _ = dialog.set_current_folder(folder);
                }
            },
            move |path| {
                if let Some(this) = weak.upgrade() {
                    this.open(path);
                }
            },
        );
    }

    fn choose_export_path(self: &Rc<Self>) {
        let Some(path) = self.document.borrow().as_ref().map(|d| d.path.clone()) else { return };
        let name = document::suggested_name(&plugins::registry(), &path);
        let weak = Rc::downgrade(self);
        dialogs::choose_file(
            &self.window,
            "Export…",
            gtk::FileChooserAction::Save,
            "Export",
            |dialog| {
                dialog.set_do_overwrite_confirmation(true);
                if let Some(folder) = path.parent() {
                    let _ = dialog.set_current_folder(folder);
                }
                dialog.set_current_name(&name);
            },
            move |path| {
                if let Some(this) = weak.upgrade() {
                    this.export(path);
                }
            },
        );
    }

    fn export(self: &Rc<Self>, path: PathBuf) {
        let Some((source, edits, original)) =
            self.document.borrow().as_ref().map(|d| (d.source.clone(), d.edits, d.path.clone()))
        else {
            return;
        };
        if path == original {
            let detail = tr("Choose another name: the original is never changed.");
            return dialogs::error(&self.window, "Could not export the image", detail);
        }
        let registry = plugins::registry();
        let weak = Rc::downgrade(self);
        let target = path.clone();
        render::in_background(
            move || document::export(&registry, &source, &edits, &target),
            move |result| {
                let Some(this) = weak.upgrade() else { return };
                if let Err(error) = result {
                    dialogs::error(&this.window, "Could not export the image", &error);
                }
            },
        );
    }

    fn open_settings(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        settings_dialog::present(
            &self.window,
            move || {
                if let Some(this) = weak.upgrade() {
                    this.reload_display_profile();
                }
            },
            || {},
        );
    }

    /// Looks up the monitor's profile off the main loop, then re-renders with it.
    fn reload_display_profile(self: &Rc<Self>) {
        let model = display_profile::monitor_model(self.window.upcast_ref());
        self.monitor.replace(model.clone());
        let weak = Rc::downgrade(self);
        render::in_background(
            move || display_profile::load(model.as_deref()),
            move |profile| {
                if let Some(this) = weak.upgrade() {
                    this.display.replace(Arc::new(DisplayTransform::new(&profile)));
                    this.refresh();
                }
            },
        );
    }
}

/// Rotate left/right and flip horizontally/vertically, as one linked group.
fn transform_buttons() -> gtk::Box {
    let group = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    group.style_context().add_class("linked");
    group.add(&icon_button("object-rotate-left-symbolic", "Rotate Left", "win.rotate-left"));
    group.add(&icon_button("object-rotate-right-symbolic", "Rotate Right", "win.rotate-right"));
    group.add(&icon_button(
        "object-flip-horizontal-symbolic",
        "Flip Horizontally",
        "win.flip-horizontal",
    ));
    group.add(&icon_button(
        "object-flip-vertical-symbolic",
        "Flip Vertically",
        "win.flip-vertical",
    ));
    group
}

/// Cancel and Apply for crop mode; hidden until cropping starts.
fn crop_bar() -> gtk::ActionBar {
    let bar = gtk::ActionBar::new();
    let cancel = gtk::Button::with_label("Cancel");
    cancel.set_action_name(Some("win.crop-cancel"));
    let apply = gtk::Button::with_label("Apply");
    apply.set_action_name(Some("win.crop-apply"));
    apply.style_context().add_class("suggested-action");
    bar.pack_start(&cancel);
    bar.pack_end(&apply);
    // The bar itself stays out of show_all; its buttons are always ready.
    cancel.show();
    apply.show();
    bar.set_no_show_all(true);
    bar
}

/// The header menu; rebuilt when the language changes, as menu models are static.
fn build_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let file = gio::Menu::new();
    file.append(Some(tr("Export…")), Some("win.export"));
    file.append(Some(tr("Revert to Original")), Some("win.revert"));
    menu.append_section(None, &file);
    let app = gio::Menu::new();
    app.append(Some(tr("Settings")), Some("win.settings"));
    menu.append_section(None, &app);
    menu
}
