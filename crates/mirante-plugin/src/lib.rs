//! Module: mirante_plugin
//! Purpose: Format plugins as separate executables that can be installed and
//! removed at runtime, for one user or for everyone.
//! Created: 2026-11-16
//! Architecture: Rust has no stable ABI and we forbid `unsafe`, so plugins are
//! processes speaking the MIRI pipe protocol instead of loaded libraries. This
//! also keeps a decoder crash on a hostile file out of the viewer. See
//! ARCHITECTURE.md, "Plugins".

pub mod defaults;
pub mod external;
pub mod manifest;
pub mod serve;
pub mod store;
pub mod wire;

pub use manifest::Manifest;
pub use serve::serve;
pub use store::{Installed, Roots, Scope};

/// Where the package installs the privileged helper that pkexec runs.
pub const ADMIN_HELPER: &str = "/usr/lib/mirante/mirante-plugin-admin";
/// File extension of plugin bundles (ustar archives).
pub const BUNDLE_EXTENSION: &str = "mirante-plugin";
