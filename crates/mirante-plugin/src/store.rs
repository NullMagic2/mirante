//! Module: mirante_plugin::store
//! Purpose: Finds, installs and removes plugins for one user or for everyone.
//! Created: 2026-11-16
//! Architecture: Three places, searched in order, first id wins: the user's
//! ~/.local/share/mirante/plugins, admin-installed /usr/local/lib/mirante/plugins,
//! and the package's /usr/lib/mirante/plugins. Package plugins are never deleted;
//! "removing" one for everyone lists it in /etc/mirante/disabled-plugins.

use crate::{
    external::ExternalPlugin,
    manifest::{self, Manifest},
};
use mirante_codec::tar;
use mirante_core::PluginRegistry;
use std::{
    collections::BTreeSet,
    env, fs, io,
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
};

const SYSTEM_ROOT: &str = "/usr/local/lib/mirante/plugins";
const BUNDLED_ROOT: &str = "/usr/lib/mirante/plugins";
const SYSTEM_DISABLED: &str = "/etc/mirante/disabled-plugins";
/// Extra package-style plugin folders, for development builds.
const PATH_VARIABLE: &str = "MIRANTE_PLUGIN_PATH";
const EXECUTABLE_MODE: u32 = 0o755;
const FILE_MODE: u32 = 0o644;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    User,
    System,
    Bundled,
}

#[derive(Clone, Debug)]
pub struct Installed {
    pub manifest: Manifest,
    pub dir: PathBuf,
    pub scope: Scope,
    pub disabled_for_me: bool,
    pub disabled_for_everyone: bool,
}

impl Installed {
    pub fn enabled(&self) -> bool {
        !self.disabled_for_me && !self.disabled_for_everyone
    }
}

#[derive(Clone, Debug)]
pub struct Roots {
    pub user: PathBuf,
    pub system: PathBuf,
    pub bundled: Vec<PathBuf>,
    pub system_disabled: PathBuf,
}

/// $XDG_DATA_HOME, or ~/.local/share.
pub fn data_home() -> PathBuf {
    env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(".local/share")
    })
}

impl Roots {
    pub fn from_env() -> Self {
        let mut bundled: Vec<PathBuf> = env::var(PATH_VARIABLE)
            .unwrap_or_default()
            .split(':')
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .collect();
        bundled.push(PathBuf::from(BUNDLED_ROOT));
        Self {
            user: data_home().join("mirante/plugins"),
            system: PathBuf::from(SYSTEM_ROOT),
            bundled,
            system_disabled: PathBuf::from(SYSTEM_DISABLED),
        }
    }

    fn searched(&self) -> Vec<(Scope, &Path)> {
        let mut roots =
            vec![(Scope::User, self.user.as_path()), (Scope::System, self.system.as_path())];
        roots.extend(self.bundled.iter().map(|root| (Scope::Bundled, root.as_path())));
        roots
    }
}

fn read_manifest(dir: &Path) -> Option<Manifest> {
    let text = fs::read_to_string(dir.join(manifest::FILE_NAME)).ok()?;
    let manifest = Manifest::parse(&text)
        .map_err(|error| eprintln!("mirante: skipping plugin in {}: {error}", dir.display()))
        .ok()?;
    (dir.file_name()? == manifest.id.as_str()).then_some(manifest)
}

pub fn system_disabled(roots: &Roots) -> BTreeSet<String> {
    let text = fs::read_to_string(&roots.system_disabled).unwrap_or_default();
    text.lines().map(str::trim).filter(|l| manifest::valid_id(l)).map(String::from).collect()
}

/// Every plugin visible to this user, sorted by name; disabled ones included.
pub fn discover(roots: &Roots, disabled_for_me: &BTreeSet<String>) -> Vec<Installed> {
    let disabled_for_everyone = system_disabled(roots);
    let mut found: Vec<Installed> = Vec::new();

    for (scope, root) in roots.searched() {
        let Ok(entries) = fs::read_dir(root) else { continue };
        let mut dirs: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        dirs.sort();
        for dir in dirs {
            let Some(manifest) = read_manifest(&dir) else { continue };
            if found.iter().any(|plugin| plugin.manifest.id == manifest.id) {
                continue;
            }
            found.push(Installed {
                disabled_for_me: disabled_for_me.contains(&manifest.id),
                disabled_for_everyone: scope != Scope::User
                    && disabled_for_everyone.contains(&manifest.id),
                manifest,
                dir,
                scope,
            });
        }
    }
    found.sort_by_key(|plugin| plugin.manifest.name.to_lowercase());
    found
}

pub fn registry(installed: &[Installed]) -> PluginRegistry {
    let mut registry = PluginRegistry::default();
    for plugin in installed.iter().filter(|p| p.enabled()) {
        let info = plugin.manifest.leak_format();
        let executable = plugin.dir.join(&plugin.manifest.executable);
        if plugin.manifest.writes {
            registry.add_encoder(ExternalPlugin { info, executable: executable.clone() });
        }
        registry.add_decoder(ExternalPlugin { info, executable });
    }
    registry
}

/// All MIME types the enabled plugins open, sorted and unique.
pub fn mime_types<'a>(installed: impl IntoIterator<Item = &'a Installed>) -> Vec<String> {
    let set: BTreeSet<&String> = installed
        .into_iter()
        .filter(|p| p.enabled())
        .flat_map(|p| &p.manifest.mime_types)
        .collect();
    set.into_iter().cloned().collect()
}

/// A relative path inside the bundle, with an optional shared top folder removed.
fn bundle_path(name: &str, top: Option<&str>) -> Option<PathBuf> {
    let name = top.and_then(|top| name.strip_prefix(top)).unwrap_or(name);
    let path = Path::new(name.trim_start_matches('/'));
    path.components().all(|c| matches!(c, Component::Normal(_))).then(|| path.to_path_buf())
}

/// The bundle's files, its manifest, and the folder prefix the manifest sits in.
fn open_bundle(bundle: &[u8]) -> Result<(Vec<tar::Entry<'_>>, Manifest, String), String> {
    let entries = tar::entries(bundle).map_err(|error| format!("not a plugin bundle: {error}"))?;
    let manifest_entry = entries
        .iter()
        .find(|e| Path::new(&e.name).file_name().is_some_and(|n| n == manifest::FILE_NAME))
        .ok_or("the bundle has no plugin.ini")?;
    let top = manifest_entry.name.strip_suffix(manifest::FILE_NAME).unwrap_or_default().to_owned();
    let manifest = Manifest::parse(&String::from_utf8_lossy(manifest_entry.data))?;
    Ok((entries, manifest, top))
}

/// Reads a bundle's manifest without installing it, to show what it is.
pub fn bundle_manifest(bundle: &[u8]) -> Result<Manifest, String> {
    open_bundle(bundle).map(|(_, manifest, _)| manifest)
}

/// Unpacks a plugin bundle (a ustar archive) into `root/<id>`, replacing any older copy.
pub fn install(bundle: &[u8], root: &Path) -> Result<Manifest, String> {
    let (entries, manifest, top) = open_bundle(bundle)?;
    let top = Some(top.as_str());

    let staging = root.join(format!(".installing-{}", manifest.id));
    let target = root.join(&manifest.id);
    let _ = fs::remove_dir_all(&staging);
    let unpack = || -> io::Result<bool> {
        let mut has_executable = false;
        for entry in &entries {
            let Some(relative) = bundle_path(&entry.name, top) else { continue };
            let path = staging.join(&relative);
            fs::create_dir_all(path.parent().expect("joined paths have parents"))?;
            fs::write(&path, entry.data)?;
            let is_executable = relative == Path::new(&manifest.executable);
            has_executable |= is_executable;
            let mode = if is_executable { EXECUTABLE_MODE } else { FILE_MODE };
            fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
        }
        Ok(has_executable)
    };
    match unpack() {
        Ok(true) => {}
        Ok(false) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(format!("the bundle has no {}", manifest.executable));
        }
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error.to_string());
        }
    }
    let _ = fs::remove_dir_all(&target);
    fs::rename(&staging, &target).map_err(|error| error.to_string())?;
    Ok(manifest)
}

pub fn remove(id: &str, root: &Path) -> Result<(), String> {
    if !manifest::valid_id(id) {
        return Err(format!("invalid plugin id {id:?}"));
    }
    fs::remove_dir_all(root.join(id)).map_err(|error| error.to_string())
}

/// Adds or removes an id in the system-wide disabled list.
pub fn set_disabled_for_everyone(roots: &Roots, id: &str, disabled: bool) -> io::Result<()> {
    let mut ids = system_disabled(roots);
    if disabled {
        ids.insert(id.to_owned())
    } else {
        ids.remove(id)
    };
    if let Some(parent) = roots.system_disabled.parent() {
        fs::create_dir_all(parent)?;
    }
    let text: String = ids.iter().map(|id| format!("{id}\n")).collect();
    fs::write(&roots.system_disabled, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str =
        "id=demo\nname=Demo\nexecutable=run\nextensions=dm\nmime-types=image/x-demo\n";

    fn temp_roots(name: &str) -> Roots {
        let base = env::temp_dir().join(format!("mirante-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        Roots {
            user: base.join("user"),
            system: base.join("system"),
            bundled: vec![base.join("bundled")],
            system_disabled: base.join("disabled"),
        }
    }

    fn bundle(files: &[(&str, &[u8])]) -> Vec<u8> {
        let files: Vec<(&str, u32, &[u8])> =
            files.iter().map(|(n, d)| (*n, FILE_MODE, *d)).collect();
        tar::write(&files)
    }

    #[test]
    fn installs_discovers_and_removes() {
        let roots = temp_roots("cycle");
        let archive =
            bundle(&[("demo/plugin.ini", MANIFEST.as_bytes()), ("demo/run", b"#!/bin/sh\n")]);
        install(&archive, &roots.user).unwrap();

        let found = discover(&roots, &BTreeSet::new());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].scope, Scope::User);
        let mode = fs::metadata(found[0].dir.join("run")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, EXECUTABLE_MODE);
        assert_eq!(mime_types(&found), ["image/x-demo"]);

        remove("demo", &roots.user).unwrap();
        assert!(discover(&roots, &BTreeSet::new()).is_empty());
    }

    #[test]
    fn user_copy_shadows_package_copy_and_disabling_works() {
        let roots = temp_roots("shadow");
        let archive = bundle(&[("plugin.ini", MANIFEST.as_bytes()), ("run", b"x")]);
        install(&archive, &roots.bundled[0]).unwrap();
        set_disabled_for_everyone(&roots, "demo", true).unwrap();
        assert!(!discover(&roots, &BTreeSet::new())[0].enabled());

        install(&archive, &roots.user).unwrap();
        let found = discover(&roots, &BTreeSet::new());
        assert_eq!((found.len(), found[0].scope, found[0].enabled()), (1, Scope::User, true));
    }

    #[test]
    fn rejects_bundles_without_their_executable_or_with_escaping_paths() {
        let roots = temp_roots("bad");
        assert!(install(&bundle(&[("plugin.ini", MANIFEST.as_bytes())]), &roots.user).is_err());
        let escaping = bundle(&[("plugin.ini", MANIFEST.as_bytes()), ("../run", b"x")]);
        assert!(install(&escaping, &roots.user).is_err());
        assert!(remove("../etc", &roots.user).is_err());
    }
}
