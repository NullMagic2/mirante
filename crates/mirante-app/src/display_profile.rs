//! Module: mirante::display_profile
//! Purpose: The display profile the user chose: the monitor's own, a standard
//! preset, or a custom ICC file.
//! Created: 2026-11-16
//! Architecture: The monitor's profile comes from colord over D-Bus (GNOME,
//! Cinnamon and MATE on all three distributions), else sRGB. An unreadable
//! custom file falls back the same way. `load` blocks; run it off the main loop.

use crate::settings;
use gtk::{gio, glib, prelude::*};
use mirante_color::{Preset, Profile};
use std::path::{Path, PathBuf};

const COLORD: &str = "org.freedesktop.ColorManager";
const COLORD_PATH: &str = "/org/freedesktop/ColorManager";
const DEVICE_INTERFACE: &str = "org.freedesktop.ColorManager.Device";
const PROFILE_INTERFACE: &str = "org.freedesktop.ColorManager.Profile";
const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";
const TIMEOUT_MS: i32 = 2000;

/// The model name of the monitor the window is on, which colord also records.
pub fn monitor_model(window: &gtk::Window) -> Option<String> {
    let gdk_window = window.window()?;
    let monitor = window.display().monitor_at_window(&gdk_window)?;
    monitor.model().map(|model| model.to_string())
}

/// What the user picked in Settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    /// The monitor's own profile from colord; sRGB when it has none.
    Monitor,
    Preset(Preset),
    File(PathBuf),
}

const PRESET_PREFIX: &str = "preset:";
const FILE_PREFIX: &str = "file:";

impl Choice {
    /// The stored choice; a bare path from older settings counts as a file.
    pub fn stored() -> Self {
        match settings::get(settings::DISPLAY_PROFILE) {
            None => Self::Monitor,
            Some(value) => match value.strip_prefix(PRESET_PREFIX).and_then(Preset::from_id) {
                Some(preset) => Self::Preset(preset),
                None => {
                    Self::File(PathBuf::from(value.strip_prefix(FILE_PREFIX).unwrap_or(&value)))
                }
            },
        }
    }

    pub fn store(&self) -> std::io::Result<()> {
        let value = match self {
            Self::Monitor => None,
            Self::Preset(preset) => Some(format!("{PRESET_PREFIX}{}", preset.id())),
            Self::File(path) => Some(format!("{FILE_PREFIX}{}", path.display())),
        };
        settings::set(settings::DISPLAY_PROFILE, value.as_deref())
    }
}

pub fn load(monitor_model: Option<&str>) -> Profile {
    let monitor =
        || colord_profile_path(monitor_model).and_then(|path| read_profile(Path::new(&path)));
    match Choice::stored() {
        Choice::Monitor => monitor().unwrap_or_else(Profile::srgb),
        Choice::Preset(preset) => preset.profile(),
        Choice::File(path) => read_profile(&path).or_else(monitor).unwrap_or_else(Profile::srgb),
    }
}

fn read_profile(path: &Path) -> Option<Profile> {
    let bytes = std::fs::read(path).ok()?;
    Profile::from_icc(&bytes)
        .map_err(|error| eprintln!("mirante: ignoring display profile {}: {error}", path.display()))
        .ok()
}

fn colord_profile_path(monitor_model: Option<&str>) -> Option<String> {
    let bus = gio::bus_get_sync(gio::BusType::System, gio::Cancellable::NONE).ok()?;
    let devices = call(&bus, COLORD_PATH, COLORD, "GetDevicesByKind", ("display",).to_variant())?;
    let devices: Vec<String> =
        devices.child_value(0).iter().filter_map(|path| path.str().map(String::from)).collect();

    // Prefer the device whose model matches; with one monitor, take it.
    let matches = |device: &&String| {
        let model = property(&bus, device, DEVICE_INTERFACE, "Model");
        monitor_model.is_some_and(|wanted| model.as_ref().and_then(|m| m.str()) == Some(wanted))
    };
    let device = devices.iter().find(matches).or(devices.first().filter(|_| devices.len() == 1))?;
    let profiles = property(&bus, device, DEVICE_INTERFACE, "Profiles")?;
    let profile = profiles.iter().next()?;
    let filename = property(&bus, profile.str()?, PROFILE_INTERFACE, "Filename")?;
    filename.str().filter(|name| !name.is_empty()).map(String::from)
}

fn call(
    bus: &gio::DBusConnection,
    path: &str,
    interface: &str,
    method: &str,
    args: glib::Variant,
) -> Option<glib::Variant> {
    bus.call_sync(
        Some(COLORD),
        path,
        interface,
        method,
        Some(&args),
        None,
        gio::DBusCallFlags::NONE,
        TIMEOUT_MS,
        gio::Cancellable::NONE,
    )
    .ok()
}

fn property(
    bus: &gio::DBusConnection,
    path: &str,
    interface: &str,
    name: &str,
) -> Option<glib::Variant> {
    let reply = call(bus, path, PROPERTIES_INTERFACE, "Get", (interface, name).to_variant())?;
    reply.child_value(0).as_variant()
}
