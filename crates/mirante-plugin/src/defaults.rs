//! Module: mirante_plugin::defaults
//! Purpose: Makes Mirante the default viewer for the types its plugins open.
//! Created: 2026-11-16
//! Architecture: Edits [Default Applications] in mimeapps.list files and the
//! .desktop entry. The value each type had before is saved, and restored when a
//! plugin (or Mirante) goes away, unless someone changed it in the meantime.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub const DESKTOP_ID: &str = "io.github.mirante.Viewer.desktop";
const DESKTOP_TEMPLATE: &str =
    include_str!("../../../packaging/io.github.mirante.Viewer.desktop.in");
const SECTION: &str = "[Default Applications]";
const SYSTEM_CONFIG: &str = "/etc/xdg";
const SYSTEM_STATE: &str = "/var/lib/mirante/previous-defaults";
pub const SYSTEM_DESKTOP_DIR: &str = "/usr/local/share/applications";

/// The .desktop entry advertising `mime_types`.
pub fn desktop_entry(mime_types: &[String]) -> String {
    let list: String = mime_types.iter().map(|m| format!("{m};")).collect();
    DESKTOP_TEMPLATE.replace("@MIME_TYPES@", &list)
}

/// Writes the entry into `dir`, or removes it when there is nothing to advertise.
pub fn write_desktop_entry(dir: &Path, mime_types: Option<&[String]>) -> io::Result<()> {
    let path = dir.join(DESKTOP_ID);
    match mime_types {
        Some(types) => {
            fs::create_dir_all(dir)?;
            fs::write(path, desktop_entry(types))
        }
        None => fs::remove_file(path)
            .or_else(|e| if e.kind() == io::ErrorKind::NotFound { Ok(()) } else { Err(e) }),
    }
}

pub fn get_default(text: &str, mime: &str) -> Option<String> {
    let mut inside = false;
    for line in text.lines() {
        if line.starts_with('[') {
            inside = line.trim() == SECTION;
        } else if inside
            && let Some(value) = line.strip_prefix(mime).and_then(|r| r.strip_prefix('='))
        {
            return Some(value.trim().to_owned());
        }
    }
    None
}

/// The same file text with `mime` set to `value`, or removed when None.
pub fn set_default(text: &str, mime: &str, value: Option<&str>) -> String {
    let (mut out, mut inside, mut done) = (String::new(), false, false);
    let entry = |value: &str| format!("{mime}={value}\n");
    for line in text.lines() {
        if line.starts_with('[') {
            if inside
                && !done
                && let Some(value) = value
            {
                out.push_str(&entry(value));
                done = true;
            }
            inside = line.trim() == SECTION;
        } else if inside && line.strip_prefix(mime).is_some_and(|rest| rest.starts_with('=')) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !done && let Some(value) = value {
        if !inside {
            out.push_str(SECTION);
            out.push('\n');
        }
        out.push_str(&entry(value));
    }
    out
}

/// Where to claim defaults and where to remember what was there.
pub struct Target {
    pub lists: Vec<PathBuf>,
    pub state: PathBuf,
    /// Replace existing defaults (system install) or only fill gaps (one user).
    pub overwrite: bool,
}

impl Target {
    /// /etc/xdg/mimeapps.list and any desktop-specific lists next to it.
    pub fn system() -> Self {
        let mut lists = vec![Path::new(SYSTEM_CONFIG).join("mimeapps.list")];
        if let Ok(entries) = fs::read_dir(SYSTEM_CONFIG) {
            let mut specific: Vec<PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.file_name().is_some_and(|n| n.to_string_lossy().ends_with("-mimeapps.list"))
                })
                .collect();
            specific.sort();
            lists.extend(specific);
        }
        Self { lists, state: PathBuf::from(SYSTEM_STATE), overwrite: true }
    }

    pub fn user(config_home: &Path, data_home: &Path) -> Self {
        Self {
            lists: vec![config_home.join("mimeapps.list")],
            state: data_home.join("mirante/previous-defaults"),
            overwrite: false,
        }
    }
}

struct Record {
    list: PathBuf,
    mime: String,
    previous: Option<String>,
}

fn read_records(state: &Path) -> Vec<Record> {
    let text = fs::read_to_string(state).unwrap_or_default();
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '|');
            let (list, mime, previous) = (parts.next()?, parts.next()?, parts.next()?);
            let previous = (!previous.is_empty()).then(|| previous.to_owned());
            Some(Record { list: PathBuf::from(list), mime: mime.to_owned(), previous })
        })
        .collect()
}

/// The loaded text of `list`, read on first use.
fn text_index(texts: &mut Vec<(PathBuf, String)>, list: &Path) -> usize {
    texts.iter().position(|(path, _)| path == list).unwrap_or_else(|| {
        texts.push((list.to_path_buf(), fs::read_to_string(list).unwrap_or_default()));
        texts.len() - 1
    })
}

/// Points `mime_types` at Mirante and hands every other recorded type back.
pub fn claim(target: &Target, mime_types: &[String]) -> io::Result<()> {
    let mut records = read_records(&target.state);
    let mut texts: Vec<(PathBuf, String)> = target
        .lists
        .iter()
        .map(|l| (l.clone(), fs::read_to_string(l).unwrap_or_default()))
        .collect();
    records.retain(|record| {
        if mime_types.contains(&record.mime) {
            return true;
        }
        let index = text_index(&mut texts, &record.list);
        let text = &mut texts[index].1;
        if get_default(text, &record.mime).as_deref() == Some(DESKTOP_ID) {
            *text = set_default(text, &record.mime, record.previous.as_deref());
        }
        false
    });
    for list in &target.lists {
        let index = text_index(&mut texts, list);
        for mime in mime_types {
            let text = &mut texts[index].1;
            let current = get_default(text, mime);
            let known = records.iter().any(|r| &r.list == list && &r.mime == mime);
            if !known {
                if !target.overwrite && current.as_deref().is_some_and(|c| c != DESKTOP_ID) {
                    continue;
                }
                records.push(Record { list: list.clone(), mime: mime.clone(), previous: current });
            }
            *text = set_default(text, mime, Some(DESKTOP_ID));
        }
    }

    for (path, text) in texts {
        if path.exists() || !text.trim().is_empty() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, text)?;
        }
    }
    if records.is_empty() {
        return fs::remove_file(&target.state)
            .or_else(|e| if e.kind() == io::ErrorKind::NotFound { Ok(()) } else { Err(e) });
    }
    if let Some(parent) = target.state.parent() {
        fs::create_dir_all(parent)?;
    }
    let lines: String = records
        .iter()
        .map(|r| {
            format!("{}|{}|{}\n", r.list.display(), r.mime, r.previous.as_deref().unwrap_or(""))
        })
        .collect();
    fs::write(&target.state, lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn set_and_get_keep_other_sections() {
        let text = "[Added Associations]\nimage/png=foo.desktop\n\n[Default Applications]\nimage/png=eog.desktop\n";
        let changed = set_default(text, "image/png", Some(DESKTOP_ID));
        assert_eq!(get_default(&changed, "image/png").as_deref(), Some(DESKTOP_ID));
        assert!(changed.contains("[Added Associations]\nimage/png=foo.desktop"));
        assert_eq!(get_default(&set_default(&changed, "image/png", None), "image/png"), None);
        assert_eq!(
            get_default(&set_default("", "image/x", Some("a")), "image/x").as_deref(),
            Some("a")
        );
    }

    #[test]
    fn claim_then_release_restores_previous_values() {
        let base = env::temp_dir().join(format!("mirante-defaults-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let list = base.join("mimeapps.list");
        fs::write(
            &list,
            "[Default Applications]\nimage/png=eog.desktop\ntext/plain=gedit.desktop\n",
        )
        .unwrap();
        let target =
            Target { lists: vec![list.clone()], state: base.join("state"), overwrite: true };
        let types = ["image/png".to_owned(), "image/jpeg".to_owned()];

        claim(&target, &types).unwrap();
        claim(&target, &types).unwrap();
        let text = fs::read_to_string(&list).unwrap();
        assert_eq!(get_default(&text, "image/jpeg").as_deref(), Some(DESKTOP_ID));

        claim(&target, &types[1..]).unwrap();
        assert_eq!(
            get_default(&fs::read_to_string(&list).unwrap(), "image/png").as_deref(),
            Some("eog.desktop")
        );

        claim(&target, &[]).unwrap();
        let text = fs::read_to_string(&list).unwrap();
        assert_eq!(get_default(&text, "image/jpeg"), None);
        assert_eq!(get_default(&text, "text/plain").as_deref(), Some("gedit.desktop"));
        assert!(!target.state.exists());
    }

    #[test]
    fn user_claims_never_override_an_existing_choice() {
        let base = env::temp_dir().join(format!("mirante-defaults-user-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let list = base.join("mimeapps.list");
        fs::write(&list, "[Default Applications]\nimage/png=gimp.desktop\n").unwrap();
        let target =
            Target { lists: vec![list.clone()], state: base.join("state"), overwrite: false };
        claim(&target, &["image/png".to_owned(), "image/x-new".to_owned()]).unwrap();
        let text = fs::read_to_string(&list).unwrap();
        assert_eq!(get_default(&text, "image/png").as_deref(), Some("gimp.desktop"));
        assert_eq!(get_default(&text, "image/x-new").as_deref(), Some(DESKTOP_ID));
    }
}
