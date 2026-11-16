//! Module: mirante_plugin::manifest
//! Purpose: plugin.ini: what a plugin is called, opens, and how to recognise its files.
//! Created: 2026-11-16
//! Architecture: Plugin executables print their manifest (`<exe> manifest`) at
//! build time, so it always matches the code. The viewer only reads manifests;
//! it starts a plugin's process when a file actually needs it.

use mirante_core::{FormatInfo, Magic};
use std::fmt::Write;

pub const FILE_NAME: &str = "plugin.ini";
const MAX_ID_LENGTH: usize = 40;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub executable: String,
    pub extensions: Vec<String>,
    pub mime_types: Vec<String>,
    /// (offset, bytes) signatures; any one matching identifies the format.
    pub magic: Vec<(usize, Vec<u8>)>,
    pub writes: bool,
}

/// Ids name directories, so only lowercase letters, digits and hyphens.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LENGTH
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

impl Manifest {
    pub fn from_format(info: &FormatInfo, executable: &str, version: &str, writes: bool) -> Self {
        Self {
            id: info.id.to_owned(),
            name: info.name.to_owned(),
            version: version.to_owned(),
            executable: executable.to_owned(),
            extensions: info.extensions.iter().map(|e| e.to_string()).collect(),
            mime_types: info.mime_types.iter().map(|m| m.to_string()).collect(),
            magic: info.magic.iter().map(|m| (m.offset, m.bytes.to_vec())).collect(),
            writes,
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut manifest = Self {
            id: String::new(),
            name: String::new(),
            version: String::new(),
            executable: String::new(),
            extensions: Vec::new(),
            mime_types: Vec::new(),
            magic: Vec::new(),
            writes: false,
        };
        let list = |value: &str, separator: char| -> Vec<String> {
            value
                .split(separator)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(String::from)
                .collect()
        };
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let (key, value) = line.split_once('=').ok_or(format!("not key=value: {line}"))?;
            let value = value.trim();
            match key.trim() {
                "id" => manifest.id = value.to_owned(),
                "name" => manifest.name = value.to_owned(),
                "version" => manifest.version = value.to_owned(),
                "executable" => manifest.executable = value.to_owned(),
                "extensions" => manifest.extensions = list(value, ';'),
                "mime-types" => manifest.mime_types = list(value, ';'),
                "writes" => manifest.writes = value == "yes",
                "magic" => {
                    for signature in list(value, ' ') {
                        let (offset, bytes) =
                            signature.split_once(':').ok_or("magic needs offset:hex")?;
                        let offset = offset.parse().map_err(|_| "bad magic offset")?;
                        manifest.magic.push((offset, unhex(bytes).ok_or("bad magic bytes")?));
                    }
                }
                _ => {}
            }
        }
        if !valid_id(&manifest.id) {
            return Err(format!("invalid plugin id {:?}", manifest.id));
        }
        let simple =
            |name: &str| !name.is_empty() && !name.contains('/') && name != "." && name != "..";
        if !simple(&manifest.executable) {
            return Err("the executable must be a plain file name".into());
        }
        Ok(manifest)
    }

    pub fn to_text(&self) -> String {
        let magic: Vec<String> =
            self.magic.iter().map(|(o, b)| format!("{o}:{}", hex(b))).collect();
        format!(
            "# Mirante image plugin\nid={}\nname={}\nversion={}\nexecutable={}\nextensions={}\nmime-types={}\nmagic={}\nwrites={}\n",
            self.id,
            self.name,
            self.version,
            self.executable,
            self.extensions.join(";"),
            self.mime_types.join(";"),
            magic.join(" "),
            if self.writes { "yes" } else { "no" },
        )
    }

    /// A FormatInfo for the registry. Leaked once per discovery; the registry
    /// lives for the whole process and manifests are a few hundred bytes.
    pub fn leak_format(&self) -> &'static FormatInfo {
        let leak_str = |s: &str| -> &'static str { Box::leak(s.to_owned().into_boxed_str()) };
        let leak_list = |items: &[String]| -> &'static [&'static str] {
            Box::leak(items.iter().map(|s| leak_str(s)).collect::<Vec<_>>().into_boxed_slice())
        };
        let magic: Vec<Magic> = self
            .magic
            .iter()
            .map(|(offset, bytes)| Magic {
                offset: *offset,
                bytes: Box::leak(bytes.clone().into_boxed_slice()),
            })
            .collect();
        Box::leak(Box::new(FormatInfo {
            id: leak_str(&self.id),
            name: leak_str(&self.name),
            extensions: leak_list(&self.extensions),
            mime_types: leak_list(&self.mime_types),
            magic: Box::leak(magic.into_boxed_slice()),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_round_trips() {
        let manifest = Manifest {
            id: "png".into(),
            name: "PNG".into(),
            version: "1.0".into(),
            executable: "mirante-plugin-png".into(),
            extensions: vec!["png".into()],
            mime_types: vec!["image/png".into()],
            magic: vec![(0, vec![0x89, b'P']), (4, b"ftyp".to_vec())],
            writes: true,
        };
        assert_eq!(Manifest::parse(&manifest.to_text()).unwrap(), manifest);
    }

    #[test]
    fn rejects_paths_and_bad_ids() {
        assert!(Manifest::parse("id=Bad Id\nexecutable=x").is_err());
        assert!(Manifest::parse("id=ok\nexecutable=../../bin/sh").is_err());
    }
}
