//! Module: mirante_plugin::external
//! Purpose: Decoder and Encoder implementations that run a plugin executable.
//! Created: 2026-11-16
//! Architecture: One short-lived process per file: bytes in on stdin, a MIRI
//! image out on stdout. A crash in a plugin is an error message, not a crash of
//! the viewer. Errors come back as "unsupported: …" or "corrupt: …" on stderr.

use crate::wire;
use mirante_core::{DecodeError, Decoder, EncodeError, Encoder, FormatInfo, Image};
use std::{
    io::{Read, Write},
    path::PathBuf,
    process::{Command, Stdio},
    thread,
};

const MAX_ERROR_TEXT: u64 = 4096;

pub struct ExternalPlugin {
    pub info: &'static FormatInfo,
    pub executable: PathBuf,
}

struct Run {
    stdout: Vec<u8>,
    stderr: String,
    success: bool,
}

impl ExternalPlugin {
    /// Feeds `input` to `<executable> <command>` and collects its output.
    fn run(
        &self,
        command: &str,
        input: &[u8],
        read: impl FnOnce(&mut dyn Read) -> Vec<u8>,
    ) -> Result<Run, String> {
        let mut child = Command::new(&self.executable)
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("cannot start {}: {error}", self.executable.display()))?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");

        // Writing and reading run at once, or a large image would fill the pipes.
        let (stdout, stderr) = thread::scope(|scope| {
            scope.spawn(move || drop(stdin.write_all(input)));
            let errors = scope.spawn(move || {
                let mut text = String::new();
                let _ = stderr.take(MAX_ERROR_TEXT).read_to_string(&mut text);
                text
            });
            let stdout = read(&mut stdout);
            (stdout, errors.join().unwrap_or_default())
        });
        let success = child.wait().map_err(|error| error.to_string())?.success();
        Ok(Run { stdout, stderr, success })
    }
}

fn first_line(text: &str) -> &str {
    text.lines().find(|line| !line.trim().is_empty()).unwrap_or("the plugin failed")
}

impl Decoder for ExternalPlugin {
    fn info(&self) -> &'static FormatInfo {
        self.info
    }

    fn decode(&self, bytes: &[u8]) -> Result<Image, DecodeError> {
        let mut decoded = None;
        let run = self
            .run("decode", bytes, |stdout| {
                decoded = Some(wire::read_image(stdout));
                Vec::new()
            })
            .map_err(DecodeError::Unsupported)?;

        match decoded {
            Some(Ok(image)) if run.success => Ok(image),
            _ => {
                let message = first_line(&run.stderr);
                Err(match message.split_once(": ") {
                    Some(("unsupported", reason)) => DecodeError::unsupported(reason),
                    Some(("corrupt", reason)) => DecodeError::corrupt(reason),
                    _ => DecodeError::corrupt(message),
                })
            }
        }
    }
}

impl Encoder for ExternalPlugin {
    fn info(&self) -> &'static FormatInfo {
        self.info
    }

    fn encode(&self, image: &Image) -> Result<Vec<u8>, EncodeError> {
        let mut input = Vec::new();
        wire::write_image(image, &mut input).map_err(|error| EncodeError(error.to_string()))?;
        let run = self
            .run("encode", &input, |stdout| {
                let mut bytes = Vec::new();
                let _ = stdout.read_to_end(&mut bytes);
                bytes
            })
            .map_err(EncodeError)?;
        if run.success {
            Ok(run.stdout)
        } else {
            Err(EncodeError(first_line(&run.stderr).to_owned()))
        }
    }
}
