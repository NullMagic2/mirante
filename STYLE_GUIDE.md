<!--
Module: mirante.documentation.style_guide
Purpose: Keep the image viewer readable, reusable and dependency-free.
Created: 2026-11-16
Architecture: ARCHITECTURE.md owns subsystem boundaries and implementation status.
Adapted from: Keyway/STYLE_GUIDE.md.
-->

# Mirante style guide

**To AI assistants:** do not add new sections to this guide unless the user
explicitly asks for it. Put new rules in the existing section they belong to,
and keep them short.

Write code that another person can follow without knowing the development
session. Prefer clear contracts, descriptive names and ordinary control flow.

Do not keep "legacy / historical" code under any circumstances, because this
encourages clutter. Keep structure identical to how a human would structure code.
README.md is a short, simple welcome file. Never turn it into a changelog or
list what changed between versions.

Subagents, always work under the current source version. Do not fork it; instead,
coordinate between yourselves which files you are working on using an agents.txt
file. This file must be deleted after the subagent closes.
DO NOT duplicate work. Before doing anything, check if anything close to what you
want to do already exists in the source and reuse it. Remove any duplicate code if
it exists.
DO NOT do A/B testing. Test only what is necessary for the code to run, and avoid
very improbable scenarios unless expressly requested.

## Language and toolchain

All crates use Rust 2024 (`edition = "2024"`) in one Cargo workspace, with Rust
1.85 as the minimum. The only third-party crate is `gtk` (gtk-rs 0.18, GTK 3).
Every codec, compressor and colour transform is ours. Do not add crates, Git
dependencies or build-time downloads.

Run `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` and
`cargo test --workspace` before delivery. `unsafe` is forbidden workspace-wide.

## Reusable code

Reusability is the main goal. Give each module one responsibility and a small
public interface. Shared primitives (byte readers, inflate, TIFF IFDs, ISO boxes)
live in `mirante-codec`; a plugin that needs another format's decoder calls that
plugin's public function instead of copying it. A new format is a new plugin
crate; never link a codec into the viewer itself. Share behaviour when the contract
is the same, but avoid generic frameworks for one caller.

Pass state explicitly. Avoid mutable global state; the language choice and the
live plugin registry are the deliberate exceptions. Keep GTK callbacks thin and put logic in testable functions.

## Comments

Every source file starts with a module comment: Module, Purpose, Created and
Architecture lines. **Module comments are at most 10 lines long.** **Any other
comment is at most 3 lines long.** Explain why, not what the next line does.

## Names and data

Use `snake_case` for functions, fields, modules and files; `PascalCase` for types;
`SCREAMING_SNAKE_CASE` for constants. Name format magic numbers, limits and tags
as constants in the module that owns them.

Treat every file as untrusted. Bound sizes before allocating, return
`DecodeError` instead of panicking, and never replace bad data with a
plausible-looking image. Report unsupported features explicitly.

## Interface

The interface must be easy to use. Do not over-explain it: no instructional
labels, no paragraphs in dialogs, no tooltips that repeat a button's label. Use
standard symbolic icons and short words. All visible text goes through `i18n::tr`.

The window uses the Yaru theme. Render lazily: only redraw or re-render when the
image, the edits or the viewport actually change, and never block the main loop
on decoding or colour work.

## Errors and tests

Return `Result` for recoverable errors. Use `expect` only for an invariant
established in code. Test decoders with small hand-built files in the test
itself; do not commit large binary fixtures.
