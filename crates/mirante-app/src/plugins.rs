//! Module: mirante::plugins
//! Purpose: The registry of installed format plugins, rebuilt when they change.
//! Created: 2026-11-16
//! Architecture: Plugins are separate executables (see mirante-plugin). This
//! module discovers them, applies the user's "off for me" list, and hands out
//! the current registry; the Settings dialog calls `reload` after changes.

use crate::settings;
use mirante_core::PluginRegistry;
use mirante_plugin::{Installed, Roots, defaults, store};
use std::{cell::RefCell, collections::BTreeSet, sync::Arc};

const ID_SEPARATOR: char = ',';

thread_local! {
    static CURRENT: RefCell<Option<Arc<PluginRegistry>>> = const { RefCell::new(None) };
}

pub fn disabled_for_me() -> BTreeSet<String> {
    let text = settings::get(settings::DISABLED_PLUGINS).unwrap_or_default();
    text.split(ID_SEPARATOR).map(str::trim).filter(|id| !id.is_empty()).map(String::from).collect()
}

pub fn set_disabled_for_me(id: &str, disabled: bool) -> std::io::Result<()> {
    let mut ids = disabled_for_me();
    if disabled {
        ids.insert(id.to_owned())
    } else {
        ids.remove(id)
    };
    let text = ids.into_iter().collect::<Vec<_>>().join(&ID_SEPARATOR.to_string());
    settings::set(settings::DISABLED_PLUGINS, (!text.is_empty()).then_some(text.as_str()))
}

pub fn installed() -> Vec<Installed> {
    store::discover(&Roots::from_env(), &disabled_for_me())
}

/// The registry for the main thread; built on first use.
pub fn registry() -> Arc<PluginRegistry> {
    CURRENT.with(|current| {
        current.borrow_mut().get_or_insert_with(|| Arc::new(store::registry(&installed()))).clone()
    })
}

/// Rediscovers plugins and refreshes this user's file associations.
pub fn reload() {
    let installed = installed();
    CURRENT.with(|current| current.replace(Some(Arc::new(store::registry(&installed)))));
    let personal: Vec<&Installed> =
        installed.iter().filter(|p| p.scope == mirante_plugin::Scope::User).collect();
    let types = store::mime_types(personal.iter().copied());
    let all_types = store::mime_types(&installed);
    let applications = store::data_home().join("applications");
    let entry = (!personal.is_empty()).then_some(all_types.as_slice());
    let target = defaults::Target::user(&gtk::glib::user_config_dir(), &store::data_home());
    if let Err(error) = defaults::write_desktop_entry(&applications, entry)
        .and_then(|()| defaults::claim(&target, &types))
    {
        eprintln!("mirante: could not update file associations: {error}");
    }
}
