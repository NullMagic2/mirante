//! Module: mirante_plugin::bin::admin
//! Purpose: System-wide plugin changes, run as root through pkexec or dpkg.
//! Created: 2026-11-16
//! Architecture: The viewer never runs as root; it asks pkexec to run this small
//! helper, which only accepts the commands below. `pack` is for building bundles
//! and needs no privileges.

use mirante_codec::tar;
use mirante_plugin::{
    Manifest, Roots, Scope,
    defaults::{self, Target},
    manifest, store,
};
use std::{collections::BTreeSet, fs, path::Path, process::ExitCode};

const USAGE: &str = "usage: mirante-plugin-admin install <bundle> | remove <id> | restore <id> \
                     | refresh | release-defaults | pack <plugin-dir> <bundle>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let roots = Roots::from_env();
    let result = match args.as_slice() {
        ["install", bundle] => install(&roots, Path::new(bundle)).and_then(|()| refresh(&roots)),
        ["remove", id] => remove(&roots, id).and_then(|()| refresh(&roots)),
        ["restore", id] => restore(&roots, id).and_then(|()| refresh(&roots)),
        ["refresh"] => refresh(&roots),
        ["release-defaults"] => defaults::claim(&Target::system(), &[]).map_err(|e| e.to_string()),
        ["pack", dir, bundle] => pack(Path::new(dir), Path::new(bundle)),
        _ => Err(USAGE.to_owned()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mirante-plugin-admin: {error}");
            ExitCode::FAILURE
        }
    }
}

fn install(roots: &Roots, bundle: &Path) -> Result<(), String> {
    let bytes = fs::read(bundle).map_err(|e| format!("{}: {e}", bundle.display()))?;
    fs::create_dir_all(&roots.system).map_err(|e| e.to_string())?;
    let manifest = store::install(&bytes, &roots.system)?;
    store::set_disabled_for_everyone(roots, &manifest.id, false).map_err(|e| e.to_string())
}

/// Deletes an admin-installed plugin; a package plugin is disabled instead.
fn remove(roots: &Roots, id: &str) -> Result<(), String> {
    let installed = store::discover(roots, &BTreeSet::new());
    let plugin = installed.iter().find(|p| p.manifest.id == id && p.scope != Scope::User);
    match plugin.map(|p| p.scope) {
        Some(Scope::System) => store::remove(id, &roots.system),
        Some(_) => store::set_disabled_for_everyone(roots, id, true).map_err(|e| e.to_string()),
        None => Err(format!("no system-wide plugin {id:?}")),
    }
}

fn restore(roots: &Roots, id: &str) -> Result<(), String> {
    store::set_disabled_for_everyone(roots, id, false).map_err(|e| e.to_string())
}

/// Re-advertises the enabled system plugins' types and claims them as defaults.
fn refresh(roots: &Roots) -> Result<(), String> {
    let installed = store::discover(roots, &BTreeSet::new());
    let shared: Vec<_> = installed.iter().filter(|p| p.scope != Scope::User).collect();
    let mime_types = store::mime_types(shared.iter().copied());
    let customised = shared.iter().any(|p| p.scope == Scope::System || !p.enabled());
    let desktop_dir = Path::new(defaults::SYSTEM_DESKTOP_DIR);
    defaults::write_desktop_entry(desktop_dir, customised.then_some(mime_types.as_slice()))
        .and_then(|()| defaults::claim(&Target::system(), &mime_types))
        .map_err(|e| e.to_string())
}

/// Bundles a plugin folder (plugin.ini plus its files) into a ustar archive.
fn pack(dir: &Path, bundle: &Path) -> Result<(), String> {
    let text = fs::read_to_string(dir.join(manifest::FILE_NAME)).map_err(|e| e.to_string())?;
    let manifest = Manifest::parse(&text)?;
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_file() {
            let name = path.file_name().expect("files have names").to_string_lossy().into_owned();
            let mode = if name == manifest.executable { 0o755 } else { 0o644 };
            files.push((
                format!("{}/{name}", manifest.id),
                mode,
                fs::read(&path).map_err(|e| e.to_string())?,
            ));
        }
    }
    files.sort();
    let borrowed: Vec<(&str, u32, &[u8])> =
        files.iter().map(|(n, m, d)| (n.as_str(), *m, d.as_slice())).collect();
    fs::write(bundle, tar::write(&borrowed)).map_err(|e| e.to_string())
}
