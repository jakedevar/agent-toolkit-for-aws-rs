//! Build and stage pipeline for per-plugin `./bin` binaries (#4487).
//!
//! The committed declaration `packaging/plugin-binaries.json` names every
//! binary a plugin ships, the workspace package and bin it is built from and
//! the platforms it is built for. [`stage`] builds those bins with locally
//! installed toolchains and copies them into the gitignored
//! `plugins/<plugin>/bin/` layout together with a generated POSIX dispatcher
//! per binary and a provenance manifest (sha256 and source commit).
//! [`launcher::check_launchers`] and [`manifest::verify_staged`] form the
//! offline packaging contract: every launcher reference in `hooks.json`,
//! `.mcp.json` and `mcp.json` resolves to a declared binary, and any staged
//! file matches its manifest entry. The design is recorded in
//! `docs/migration/plugin-binaries.md`.

pub mod declaration;
pub mod feature_graph;
pub mod launcher;
pub mod manifest;
pub mod platform;
pub mod stage;

pub use declaration::{BinarySpec, Declaration, Destination, PlatformSpec, Role, DECLARATION_PATH};
pub use platform::Platform;

/// Name of the generated staging manifest inside each plugin `bin/` directory.
pub const MANIFEST_FILE: &str = "manifest.json";

/// Name of the per-plugin directory that holds staged binaries.
pub const BIN_DIR: &str = "bin";
