//! Module: mirante_plugin::serve
//! Purpose: The `main` of every plugin executable.
//! Created: 2026-11-16
//! Architecture: A plugin crate's main.rs is one line calling `serve` with its
//! `register` function. Commands: `manifest`, `decode` (file on stdin, MIRI on
//! stdout) and `encode` (MIRI on stdin, file on stdout).

use crate::{manifest::Manifest, wire};
use mirante_core::{DecodeError, PluginRegistry};
use std::{
    io::{self, Read, Write},
    process::ExitCode,
};

/// Largest input file a plugin accepts on stdin.
const MAX_INPUT: u64 = 2 << 30;

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("{message}");
    ExitCode::FAILURE
}

fn describe(error: &DecodeError) -> String {
    match error {
        DecodeError::Unsupported(reason) => format!("unsupported: {reason}"),
        other => format!("corrupt: {other}"),
    }
}

pub fn serve(register: fn(&mut PluginRegistry), version: &str) -> ExitCode {
    let mut registry = PluginRegistry::default();
    register(&mut registry);
    let Some(info) = registry.readable_formats().next() else {
        return fail("unsupported: the plugin registered no format");
    };
    let mut input = Vec::new();
    let command = std::env::args().nth(1).unwrap_or_default();

    match command.as_str() {
        "manifest" => {
            let executable = std::env::current_exe().ok();
            let name = executable.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy());
            let writes = registry.encoder(info.id).is_some();
            print!(
                "{}",
                Manifest::from_format(info, &name.unwrap_or_default(), version, writes).to_text()
            );
            ExitCode::SUCCESS
        }
        "decode" => {
            if let Err(error) = io::stdin().take(MAX_INPUT).read_to_end(&mut input) {
                return fail(format!("corrupt: {error}"));
            }
            let decoder =
                registry.decoder(info.id).expect("the format was registered with a decoder");
            match decoder.decode(&input) {
                Ok(image) => match wire::write_image(&image, &mut io::stdout().lock()) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => fail(format!("corrupt: {error}")),
                },
                Err(error) => fail(describe(&error)),
            }
        }
        "encode" => {
            let Some(encoder) = registry.encoder(info.id) else {
                return fail("unsupported: this plugin cannot write files");
            };
            let image = match wire::read_image(&mut io::stdin().lock()) {
                Ok(image) => image,
                Err(error) => return fail(describe(&error)),
            };
            match encoder.encode(&image) {
                Ok(bytes) => match io::stdout().lock().write_all(&bytes) {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => fail(error),
                },
                Err(error) => fail(error),
            }
        }
        _ => fail("usage: <plugin> manifest | decode | encode"),
    }
}
