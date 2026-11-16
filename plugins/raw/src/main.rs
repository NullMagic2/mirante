//! Module: mirante_format_raw::main
//! Purpose: The plugin executable the viewer starts for each file of this format.
//! Created: 2026-11-16
//! Architecture: All protocol handling lives in mirante_plugin::serve.

fn main() -> std::process::ExitCode {
    mirante_plugin::serve(mirante_format_raw::register, env!("CARGO_PKG_VERSION"))
}
