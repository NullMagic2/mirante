//! Module: mirante::toolbar
//! Purpose: The bottom bar: large buttons for everything done to a picture.
//! Created: 2026-11-16
//! Architecture: Three groups (browse; rotate, flip, crop, adjust; zoom and
//! save), all driven by window actions. While cropping, the bar swaps to Cancel
//! and Apply. Only language and settings stay in the title bar.

use crate::style::Tint;
use gtk::{cairo, prelude::*};

const TOOLS_PAGE: &str = "tools";
const CROP_PAGE: &str = "crop";
const GROUP_SPACING: i32 = 4;
const BAR_SPACING: i32 = 12;
/// Logical size of the toolbar icons, matching IconSize::LargeToolbar.
const ICON_SIZE: i32 = 24;

pub struct Toolbar {
    pub root: gtk::Stack,
    pub crop_button: gtk::ToggleButton,
    pub adjust_button: gtk::MenuButton,
}

impl Toolbar {
    pub fn new() -> Self {
        let crop_button = gtk::ToggleButton::new();
        dress(&crop_button, "image-crop-symbolic", "Crop", Tint::Orange);
        let adjust_button = gtk::MenuButton::new();
        dress(&adjust_button, "preferences-color-symbolic", "Adjust", Tint::Neutral);
        adjust_button.set_image(Some(&triad()));

        let browse = group(&[
            tool("document-open-symbolic", "Open", "win.open", Tint::Blue),
            tool("go-previous-symbolic", "Previous", "win.previous", Tint::Blue),
            tool("go-next-symbolic", "Next", "win.next", Tint::Blue),
        ]);
        let edit = group(&[
            tool("object-rotate-left-symbolic", "Rotate Left", "win.rotate-left", Tint::Purple),
            tool("object-rotate-right-symbolic", "Rotate Right", "win.rotate-right", Tint::Purple),
            tool(
                "object-flip-horizontal-symbolic",
                "Flip Horizontally",
                "win.flip-horizontal",
                Tint::Purple,
            ),
            tool(
                "object-flip-vertical-symbolic",
                "Flip Vertically",
                "win.flip-vertical",
                Tint::Purple,
            ),
        ]);
        edit.add(&gtk::Separator::new(gtk::Orientation::Vertical));
        edit.add(&crop_button);
        edit.add(&adjust_button);
        let output = group(&[
            tool("zoom-out-symbolic", "Zoom Out", "win.zoom-out", Tint::Teal),
            tool("zoom-fit-best-symbolic", "Best Fit", "win.zoom-fit", Tint::Teal),
            tool("zoom-in-symbolic", "Zoom In", "win.zoom-in", Tint::Teal),
        ]);
        output.add(&gtk::Separator::new(gtk::Orientation::Vertical));
        output.add(&tool(
            "document-revert-symbolic",
            "Revert to Original",
            "win.revert",
            Tint::Red,
        ));
        output.add(&tool("document-save-as-symbolic", "Export…", "win.export", Tint::Green));

        let tools = bar();
        tools.pack_start(&browse, false, false, 0);
        tools.set_center_widget(Some(&edit));
        tools.pack_end(&output, false, false, 0);

        let cropping = bar();
        let cancel = gtk::Button::with_label("Cancel");
        cancel.set_action_name(Some("win.crop-cancel"));
        let apply = gtk::Button::with_label("Apply");
        apply.set_action_name(Some("win.crop-apply"));
        apply.style_context().add_class("suggested-action");
        for button in [&cancel, &apply] {
            button.style_context().add_class("mirante-wide");
        }
        cropping.pack_start(&cancel, false, false, 0);
        cropping.pack_end(&apply, false, false, 0);

        let root = gtk::Stack::new();
        root.set_transition_type(gtk::StackTransitionType::Crossfade);
        root.add_named(&tools, TOOLS_PAGE);
        root.add_named(&cropping, CROP_PAGE);
        Self { root, crop_button, adjust_button }
    }

    pub fn set_cropping(&self, cropping: bool) {
        self.root.set_visible_child_name(if cropping { CROP_PAGE } else { TOOLS_PAGE });
    }
}

fn bar() -> gtk::Box {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, BAR_SPACING);
    bar.style_context().add_class("mirante-toolbar");
    bar
}

fn group(buttons: &[gtk::Button]) -> gtk::Box {
    let group = gtk::Box::new(gtk::Orientation::Horizontal, GROUP_SPACING);
    buttons.iter().for_each(|button| group.add(button));
    group
}

/// A large, flat icon button that runs a window action.
fn tool(icon: &str, tooltip: &str, action: &str, tint: Tint) -> gtk::Button {
    let button = gtk::Button::new();
    dress(&button, icon, tooltip, tint);
    button.set_action_name(Some(action));
    button
}

fn dress(
    button: &(impl IsA<gtk::Button> + IsA<gtk::Widget>),
    icon: &str,
    tooltip: &str,
    tint: Tint,
) {
    let image = gtk::Image::from_icon_name(Some(icon), gtk::IconSize::LargeToolbar);
    button.set_image(Some(&image));
    button.set_tooltip_text(Some(tooltip));
    button.set_relief(gtk::ReliefStyle::None);
    button.style_context().add_class("mirante-tool");
    button.style_context().add_class(tint.class());
}

/// Red, green and blue discs mixed additively: the colour-correction icon.
fn triad() -> gtk::DrawingArea {
    const RADIUS: f64 = 6.6;
    const SPREAD: f64 = 3.9;
    const DISCS: [(f64, [f64; 3]); 3] =
        [(-90.0, [0.93, 0.20, 0.20]), (150.0, [0.18, 0.80, 0.35]), (30.0, [0.22, 0.45, 0.95])];
    let area = gtk::DrawingArea::new();
    area.set_size_request(ICON_SIZE, ICON_SIZE);
    area.set_valign(gtk::Align::Center);
    area.connect_draw(|area, cr| {
        let centre =
            (f64::from(area.allocated_width()) / 2.0, f64::from(area.allocated_height()) / 2.0);
        cr.push_group();
        cr.set_operator(cairo::Operator::Add);
        for (angle, [r, g, b]) in DISCS {
            let radians = angle.to_radians();
            let (x, y) = (centre.0 + SPREAD * radians.cos(), centre.1 + SPREAD * radians.sin());
            cr.set_source_rgba(r, g, b, 1.0);
            cr.arc(x, y, RADIUS, 0.0, std::f64::consts::TAU);
            let _ = cr.fill();
        }
        if cr.pop_group_to_source().is_ok() {
            let _ = cr.paint();
        }
        gtk::glib::Propagation::Stop
    });
    area
}
