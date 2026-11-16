//! Module: mirante::icons
//! Purpose: Turns working-space images into Cairo surfaces and GTK images.
//! Created: 2026-11-16
//! Architecture: Bundled PNGs (the flags) go through our own PNG plugin and the
//! same display path as photos, so there is one way pixels reach the screen.

use gtk::{cairo, prelude::*};
use mirante_color::{DisplayTransform, Profile};
use mirante_core::Image;

/// Wraps premultiplied ARGB32 bytes, as DisplayTransform produces them.
pub fn surface(width: u32, height: u32, argb: Vec<u8>) -> Option<cairo::ImageSurface> {
    let stride = width as i32 * 4;
    cairo::ImageSurface::create_for_data(
        argb,
        cairo::Format::ARgb32,
        width as i32,
        height as i32,
        stride,
    )
    .map_err(|error| eprintln!("mirante: cannot create a surface: {error}"))
    .ok()
}

pub fn image_surface(image: &Image, display: &DisplayTransform) -> Option<cairo::ImageSurface> {
    surface(image.width(), image.height(), display.to_cairo_argb32(image))
}

/// A crisp image of a bundled PNG at `width` logical pixels on any scale factor.
pub fn png_image(bytes: &'static [u8], width: i32) -> gtk::Image {
    let widget = gtk::Image::new();
    let render = move |widget: &gtk::Image| {
        let scale = widget.scale_factor();
        let Ok(decoded) = mirante_format_png::decode(bytes) else { return };
        let working = mirante_color::to_working(decoded);
        let size = (width * scale) as u32;
        let small = mirante_edit::downscale_to_fit(&working, size, size);
        if let Some(surface) = image_surface(&small, &DisplayTransform::new(&Profile::srgb())) {
            surface.set_device_scale(f64::from(scale), f64::from(scale));
            widget.set_from_surface(Some(&surface));
        }
    };
    render(&widget);
    widget.connect_scale_factor_notify(render);
    widget
}
