//! Module: mirante::render
//! Purpose: Runs slow work off the main loop and coalesces preview renders.
//! Created: 2026-11-16
//! Architecture: At most one preview render runs; while it does, only the most
//! recent request is kept. Dragging a slider therefore never queues up work,
//! and the screen always ends on the latest edits.

use gtk::{gio, glib};
use mirante_color::DisplayTransform;
use mirante_core::Image;
use mirante_edit::Edits;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

/// Runs `work` on a worker thread and hands its result to `done` on the main loop.
pub fn in_background<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) + 'static,
) {
    glib::spawn_future_local(async move {
        match gio::spawn_blocking(work).await {
            Ok(result) => done(result),
            Err(_) => eprintln!("mirante: a background task panicked"),
        }
    });
}

#[derive(Clone)]
pub struct Job {
    pub source: Arc<Image>,
    pub edits: Edits,
    /// Device pixels available; the result is never larger than the source.
    pub max_size: (u32, u32),
    pub display: Arc<DisplayTransform>,
}

/// Premultiplied ARGB32 pixels ready to wrap in a Cairo surface.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub argb: Vec<u8>,
}

fn run(job: Job) -> Frame {
    let image = job.edits.apply(&job.source, Some(job.max_size));
    Frame {
        width: image.width(),
        height: image.height(),
        argb: job.display.to_cairo_argb32(&image),
    }
}

pub struct Renderer {
    busy: Cell<bool>,
    pending: RefCell<Option<Job>>,
    deliver: Box<dyn Fn(Frame)>,
}

impl Renderer {
    pub fn new(deliver: impl Fn(Frame) + 'static) -> Rc<Self> {
        Rc::new(Self {
            busy: Cell::new(false),
            pending: RefCell::new(None),
            deliver: Box::new(deliver),
        })
    }

    pub fn request(self: &Rc<Self>, job: Job) {
        if self.busy.replace(true) {
            self.pending.replace(Some(job));
            return;
        }
        let renderer = self.clone();
        in_background(
            move || run(job),
            move |frame| {
                renderer.busy.set(false);
                (renderer.deliver)(frame);
                if let Some(next) = renderer.pending.take() {
                    renderer.request(next);
                }
            },
        );
    }
}
