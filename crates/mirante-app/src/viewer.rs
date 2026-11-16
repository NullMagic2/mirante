//! Module: mirante::viewer
//! Purpose: The canvas: draws the rendered frame, zooms, and drives cropping.
//! Created: 2026-11-16
//! Architecture: Knows image geometry but not pixels' origin; the window feeds
//! it frames and asks it what size to render. The wheel zooms around the
//! pointer; a zoomed view pans by dragging or Shift+wheel, through the
//! surrounding ScrolledWindow.

use crate::crop_tool::{self, Grip, HANDLE_REACH, Rect};
use crate::icons;
use crate::render::Frame;
use gtk::{cairo, gdk, prelude::*};
use mirante_edit::CropRect;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

const ZOOM_STEP: f64 = 1.25;
const ZOOM_RANGE: (f64, f64) = (0.05, 16.0);
/// Darkening outside the crop selection.
const CROP_SHADE: f64 = 0.55;
/// Side of the square handles drawn on the selection, in screen pixels.
const HANDLE_SIZE: f64 = 8.0;

/// A grab in progress: what was grabbed, the selection then, the press point.
type CropDrag = (Grip, Rect, (f64, f64));

#[derive(Default)]
struct Crop {
    active: bool,
    /// Size of the image being cropped, in source pixels.
    bounds: (u32, u32),
    selection: Option<Rect>,
    drag: Option<CropDrag>,
}

pub struct Viewer {
    pub scrolled: gtk::ScrolledWindow,
    area: gtk::DrawingArea,
    surface: RefCell<Option<cairo::ImageSurface>>,
    /// Size in source pixels of what is shown (the crop, unless cropping).
    image_size: Cell<Option<(u32, u32)>>,
    /// None fits the image to the window.
    zoom: Cell<Option<f64>>,
    crop: RefCell<Crop>,
    /// Pointer (root coordinates) and scroll position when a pan drag began.
    pan: Cell<Option<(f64, f64, f64, f64)>>,
    on_resize: RefCell<Option<Box<dyn Fn()>>>,
}

/// Where the image sits in the widget: offset and widget pixels per source pixel.
struct Placement {
    x: f64,
    y: f64,
    scale: f64,
}

impl Viewer {
    pub fn new() -> Rc<Self> {
        let area = gtk::DrawingArea::new();
        area.add_events(
            gdk::EventMask::BUTTON_PRESS_MASK
                | gdk::EventMask::BUTTON_RELEASE_MASK
                | gdk::EventMask::POINTER_MOTION_MASK
                | gdk::EventMask::SCROLL_MASK
                | gdk::EventMask::SMOOTH_SCROLL_MASK,
        );
        let scrolled = gtk::ScrolledWindow::new(gtk::Adjustment::NONE, gtk::Adjustment::NONE);
        scrolled.add(&area);
        let viewer = Rc::new(Self {
            scrolled,
            area,
            surface: RefCell::new(None),
            image_size: Cell::new(None),
            zoom: Cell::new(None),
            crop: RefCell::new(Crop::default()),
            pan: Cell::new(None),
            on_resize: RefCell::new(None),
        });
        viewer.connect_signals();
        viewer
    }

    fn connect_signals(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.area.connect_draw(move |area, cr| {
            if let Some(viewer) = weak.upgrade() {
                viewer.draw(area, cr);
            }
            gtk::glib::Propagation::Stop
        });
        let weak = Rc::downgrade(self);
        self.area.connect_size_allocate(move |_, _| {
            if let Some(viewer) = weak.upgrade() {
                viewer.notify_resize();
            }
        });
        let weak = Rc::downgrade(self);
        self.area.connect_scroll_event(move |_, event| {
            let Some(viewer) = weak.upgrade() else { return gtk::glib::Propagation::Proceed };
            // Shift+wheel keeps the usual scrolling, for panning without a drag.
            if event.state().contains(gdk::ModifierType::SHIFT_MASK) {
                return gtk::glib::Propagation::Proceed;
            }
            let steps = match event.direction() {
                gdk::ScrollDirection::Up => -1.0,
                gdk::ScrollDirection::Down => 1.0,
                gdk::ScrollDirection::Smooth => event.delta().1,
                _ => return gtk::glib::Propagation::Proceed,
            };
            viewer.zoom_at(ZOOM_STEP.powf(-steps), event.position());
            gtk::glib::Propagation::Stop
        });
        let weak = Rc::downgrade(self);
        self.area.connect_button_press_event(move |_, event| {
            if let Some(viewer) = weak.upgrade() {
                viewer.crop_press(event.position());
                viewer.pan_press(event.root());
            }
            gtk::glib::Propagation::Proceed
        });
        let weak = Rc::downgrade(self);
        self.area.connect_motion_notify_event(move |_, event| {
            if let Some(viewer) = weak.upgrade() {
                viewer.crop_drag(event.position());
                viewer.pan_drag(event.root());
            }
            gtk::glib::Propagation::Proceed
        });
        let weak = Rc::downgrade(self);
        self.area.connect_button_release_event(move |_, _| {
            if let Some(viewer) = weak.upgrade() {
                viewer.crop_release();
                viewer.pan.set(None);
            }
            gtk::glib::Propagation::Proceed
        });
    }

    fn notify_resize(&self) {
        if let Some(callback) = self.on_resize.borrow().as_ref() {
            callback();
        }
    }

    /// Called when the space to render into changes (window size, zoom, scale).
    pub fn connect_resize(&self, callback: impl Fn() + 'static) {
        self.on_resize.replace(Some(Box::new(callback)));
    }

    /// Sets what is being shown, in source pixels; None clears the canvas.
    pub fn set_image_size(&self, size: Option<(u32, u32)>) {
        if self.image_size.replace(size) != size {
            self.surface.replace(None);
            self.apply_zoom();
        }
    }

    pub fn show_frame(&self, frame: Frame) {
        self.surface.replace(icons::surface(frame.width, frame.height, frame.argb));
        self.area.queue_draw();
    }

    fn placement(&self) -> Option<Placement> {
        let (width, height) = self.image_size.get()?;
        let (width, height) = (f64::from(width), f64::from(height));
        let (area_w, area_h) =
            (f64::from(self.area.allocated_width()), f64::from(self.area.allocated_height()));
        let fit = (area_w / width).min(area_h / height).min(1.0);
        let scale = self.zoom.get().unwrap_or(fit);
        let x = ((area_w - width * scale) / 2.0).max(0.0);
        let y = ((area_h - height * scale) / 2.0).max(0.0);
        Some(Placement { x, y, scale })
    }

    /// Device pixels the shown image occupies; renders larger than this are wasted.
    pub fn render_size(&self) -> Option<(u32, u32)> {
        let (width, height) = self.image_size.get()?;
        let placement = self.placement()?;
        let factor = placement.scale * f64::from(self.area.scale_factor());
        let size = |v: u32| ((f64::from(v) * factor).ceil() as u32).max(1);
        Some((size(width), size(height)))
    }

    fn draw(&self, area: &gtk::DrawingArea, cr: &cairo::Context) {
        let context = area.style_context();
        gtk::render_background(
            &context,
            cr,
            0.0,
            0.0,
            area.allocated_width().into(),
            area.allocated_height().into(),
        );
        let (Some(placement), Some((width, height))) = (self.placement(), self.image_size.get())
        else {
            return;
        };
        let (shown_w, shown_h) =
            (f64::from(width) * placement.scale, f64::from(height) * placement.scale);

        if let Some(surface) = self.surface.borrow().as_ref() {
            let _ = cr.save();
            cr.translate(placement.x, placement.y);
            cr.scale(shown_w / f64::from(surface.width()), shown_h / f64::from(surface.height()));
            let _ = cr.set_source_surface(surface, 0.0, 0.0);
            cr.source().set_filter(cairo::Filter::Good);
            let _ = cr.paint();
            let _ = cr.restore();
        }
        let crop = self.crop.borrow();
        if let Some(selection) = crop.selection.filter(|_| crop.active) {
            draw_crop(cr, &placement, (shown_w, shown_h), selection);
        }
    }

    /// Zooms around the middle of what is visible.
    pub fn zoom_by(self: &Rc<Self>, factor: f64) {
        let (h, v) = (self.scrolled.hadjustment(), self.scrolled.vadjustment());
        let centre = (h.value() + h.page_size() / 2.0, v.value() + v.page_size() / 2.0);
        self.zoom_at(factor, centre);
    }

    /// Zooms so the image point under `pointer` (area coordinates) stays put.
    /// Zooming out past the fitted size returns to fitting the window.
    pub fn zoom_at(self: &Rc<Self>, factor: f64, pointer: (f64, f64)) {
        let (Some(placement), Some((width, height))) = (self.placement(), self.image_size.get())
        else {
            return;
        };
        let viewport = (
            f64::from(self.scrolled.allocated_width()),
            f64::from(self.scrolled.allocated_height()),
        );
        let fit = (viewport.0 / f64::from(width)).min(viewport.1 / f64::from(height)).min(1.0);
        let scale = (placement.scale * factor).clamp(ZOOM_RANGE.0, ZOOM_RANGE.1);
        let (h, v) = (self.scrolled.hadjustment(), self.scrolled.vadjustment());
        let image_point = (
            (pointer.0 - placement.x) / placement.scale,
            (pointer.1 - placement.y) / placement.scale,
        );
        let on_screen = (pointer.0 - h.value(), pointer.1 - v.value());

        self.zoom.set((scale > fit).then_some(scale));
        self.apply_zoom();
        // The new size is only laid out after this handler returns.
        let weak = Rc::downgrade(self);
        gtk::glib::idle_add_local_once(move || {
            let Some(placement) = weak.upgrade().and_then(|viewer| viewer.placement()) else {
                return;
            };
            h.set_value(image_point.0 * placement.scale + placement.x - on_screen.0);
            v.set_value(image_point.1 * placement.scale + placement.y - on_screen.1);
        });
    }

    /// Starts dragging a zoomed image around; cropping uses the drag instead.
    fn pan_press(&self, (x, y): (f64, f64)) {
        if self.zoom.get().is_some() && !self.crop.borrow().active {
            let (h, v) = (self.scrolled.hadjustment(), self.scrolled.vadjustment());
            self.pan.set(Some((x, y, h.value(), v.value())));
        }
    }

    fn pan_drag(&self, (x, y): (f64, f64)) {
        if let Some((start_x, start_y, left, top)) = self.pan.get() {
            self.scrolled.hadjustment().set_value(left - (x - start_x));
            self.scrolled.vadjustment().set_value(top - (y - start_y));
        }
    }

    /// None fits the window; Some(1.0) is one source pixel per screen pixel.
    pub fn set_zoom(&self, zoom: Option<f64>) {
        self.zoom.set(zoom);
        self.apply_zoom();
    }

    fn apply_zoom(&self) {
        match (self.zoom.get(), self.image_size.get()) {
            (Some(zoom), Some((width, height))) => {
                let size = |v: u32| (f64::from(v) * zoom).round() as i32;
                self.area.set_size_request(size(width), size(height));
            }
            _ => self.area.set_size_request(-1, -1),
        }
        self.area.queue_resize();
        self.area.queue_draw();
        self.notify_resize();
    }

    /// Enters crop mode at fit-to-window; the selection starts as the current
    /// crop, or the whole image so its edges can simply be dragged in.
    pub fn begin_crop(&self, crop: Option<CropRect>, bounds: (u32, u32)) {
        let selection = crop.map_or(Rect::whole(bounds), Rect::from_crop);
        self.crop.replace(Crop { active: true, bounds, selection: Some(selection), drag: None });
        self.set_zoom(None);
    }

    /// Leaves crop mode; returns the crop to apply (None means the whole image).
    pub fn end_crop(&self) -> Option<CropRect> {
        let crop = self.crop.replace(Crop::default());
        self.set_cursor(None);
        self.area.queue_draw();
        crop.selection.and_then(|rect| rect.to_crop(crop.bounds))
    }

    /// Widget to source pixels, unclamped so drags may run past the edges.
    fn to_image(&self, (x, y): (f64, f64)) -> Option<(f64, f64)> {
        let placement = self.placement()?;
        Some(((x - placement.x) / placement.scale, (y - placement.y) / placement.scale))
    }

    fn reach(&self) -> f64 {
        HANDLE_REACH / self.placement().map_or(1.0, |p| p.scale)
    }

    fn crop_press(&self, position: (f64, f64)) {
        let (Some(point), reach) = (self.to_image(position), self.reach()) else { return };
        let mut crop = self.crop.borrow_mut();
        if let (true, Some(selection)) = (crop.active, crop.selection) {
            crop.drag = Some((crop_tool::grip(selection, point, reach), selection, point));
        }
    }

    /// Moves or resizes while dragging; otherwise shows what a press would grab.
    fn crop_drag(&self, position: (f64, f64)) {
        let (Some(point), reach) = (self.to_image(position), self.reach()) else { return };
        let mut crop = self.crop.borrow_mut();
        if !crop.active {
            return;
        }
        match crop.drag {
            Some((grip, original, start)) => {
                crop.selection = Some(crop_tool::drag(grip, original, start, point, crop.bounds));
                drop(crop);
                self.area.queue_draw();
            }
            None => {
                let hover =
                    crop.selection.map_or(Grip::New, |rect| crop_tool::grip(rect, point, reach));
                drop(crop);
                self.set_cursor(Some(crop_tool::cursor_name(hover)));
            }
        }
    }

    fn crop_release(&self) {
        let mut crop = self.crop.borrow_mut();
        if let (Some((grip, original, _)), Some(result)) = (crop.drag.take(), crop.selection) {
            crop.selection = Some(crop_tool::finish(grip, original, result));
            drop(crop);
            self.area.queue_draw();
        }
    }

    fn set_cursor(&self, name: Option<&str>) {
        if let Some(window) = self.area.window() {
            let cursor = name.and_then(|name| gdk::Cursor::from_name(&window.display(), name));
            window.set_cursor(cursor.as_ref());
        }
    }
}

/// Shades outside the selection; draws its border, rule-of-thirds lines and handles.
fn draw_crop(
    cr: &cairo::Context,
    placement: &Placement,
    (shown_w, shown_h): (f64, f64),
    rect: Rect,
) {
    let s = placement.scale;
    let (x, y) = (placement.x + rect.x * s, placement.y + rect.y * s);
    let (w, h) = (rect.width * s, rect.height * s);

    cr.set_fill_rule(cairo::FillRule::EvenOdd);
    cr.rectangle(placement.x, placement.y, shown_w, shown_h);
    cr.rectangle(x, y, w, h);
    cr.set_source_rgba(0.0, 0.0, 0.0, CROP_SHADE);
    let _ = cr.fill();

    cr.set_source_rgba(1.0, 1.0, 1.0, 0.9);
    cr.set_line_width(1.0);
    for third in [1.0, 2.0] {
        cr.move_to(x + w * third / 3.0, y);
        cr.line_to(x + w * third / 3.0, y + h);
        cr.move_to(x, y + h * third / 3.0);
        cr.line_to(x + w, y + h * third / 3.0);
    }
    cr.rectangle(x + 0.5, y + 0.5, w - 1.0, h - 1.0);
    let _ = cr.stroke();

    let half = HANDLE_SIZE / 2.0;
    for hx in [x, x + w / 2.0, x + w] {
        for hy in [y, y + h / 2.0, y + h] {
            if hx == x + w / 2.0 && hy == y + h / 2.0 {
                continue;
            }
            cr.rectangle(hx - half, hy - half, HANDLE_SIZE, HANDLE_SIZE);
        }
    }
    cr.set_source_rgb(1.0, 1.0, 1.0);
    let _ = cr.fill_preserve();
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.6);
    let _ = cr.stroke();
}
