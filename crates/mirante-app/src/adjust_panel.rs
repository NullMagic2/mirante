//! Module: mirante::adjust_panel
//! Purpose: The colour-correction popover: five sliders and a reset button.
//! Created: 2026-11-16
//! Architecture: Reports every change as a full Adjustments value; the window
//! decides what to re-render. No values or hints are shown, by design.

use gtk::prelude::*;
use mirante_edit::Adjustments;
use std::{cell::Cell, rc::Rc};

const SLIDER_WIDTH: i32 = 220;
const SPACING: i32 = 8;
const EXPOSURE_STOPS: f64 = 3.0;
const STEP: f64 = 0.01;

pub struct AdjustPanel {
    pub popover: gtk::Popover,
    sliders: [gtk::Scale; 5],
    updating: Cell<bool>,
}

impl AdjustPanel {
    pub fn new(
        relative_to: &impl IsA<gtk::Widget>,
        on_change: impl Fn(Adjustments) + 'static,
    ) -> Rc<Self> {
        let popover = gtk::Popover::new(Some(relative_to));
        let grid = gtk::Grid::new();
        grid.set_row_spacing(SPACING as u32);
        grid.set_column_spacing(SPACING as u32 * 2);
        grid.set_border_width(SPACING as u32 * 2);

        let names = ["Exposure", "Contrast", "Saturation", "Temperature", "Tint"];
        let sliders = names.map(|name| {
            let range = if name == "Exposure" { EXPOSURE_STOPS } else { 1.0 };
            let slider = gtk::Scale::with_range(gtk::Orientation::Horizontal, -range, range, STEP);
            slider.set_draw_value(false);
            slider.set_value(0.0);
            slider.add_mark(0.0, gtk::PositionType::Bottom, None);
            slider.set_size_request(SLIDER_WIDTH, -1);
            slider
        });
        for (row, (name, slider)) in names.iter().zip(&sliders).enumerate() {
            let label = gtk::Label::new(Some(name));
            label.set_xalign(0.0);
            grid.attach(&label, 0, row as i32, 1, 1);
            grid.attach(slider, 1, row as i32, 1, 1);
        }
        let reset = gtk::Button::with_label("Reset");
        reset.set_halign(gtk::Align::End);
        grid.attach(&reset, 1, names.len() as i32, 1, 1);
        popover.add(&grid);
        grid.show_all();
        crate::i18n::localize(&grid);

        let panel = Rc::new(Self { popover, sliders, updating: Cell::new(false) });
        let on_change = Rc::new(on_change);
        for slider in &panel.sliders {
            let (weak, on_change) = (Rc::downgrade(&panel), on_change.clone());
            slider.connect_value_changed(move |_| {
                let Some(panel) = weak.upgrade() else { return };
                if !panel.updating.get() {
                    on_change(panel.value());
                }
            });
        }
        let weak = Rc::downgrade(&panel);
        reset.connect_clicked(move |_| {
            if let Some(panel) = weak.upgrade() {
                panel.set(Adjustments::default());
                on_change(Adjustments::default());
            }
        });
        panel
    }

    fn value(&self) -> Adjustments {
        let [exposure, contrast, saturation, temperature, tint] =
            self.sliders.each_ref().map(|s| s.value() as f32);
        Adjustments { exposure, contrast, saturation, temperature, tint }
    }

    /// Shows a document's adjustments without reporting them back as a change.
    pub fn set(&self, adjustments: Adjustments) {
        self.updating.set(true);
        let values = [
            adjustments.exposure,
            adjustments.contrast,
            adjustments.saturation,
            adjustments.temperature,
            adjustments.tint,
        ];
        for (slider, value) in self.sliders.iter().zip(values) {
            slider.set_value(f64::from(value));
        }
        self.updating.set(false);
    }
}
