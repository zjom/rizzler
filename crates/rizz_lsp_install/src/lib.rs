//! Declarative installer for LSP server binaries.
//!
//! `rizz_lsp` runs language servers but doesn't know where their binaries
//! come from. This crate answers that question:
//!
//! 1. Parse `lsp.toml` into a [`Manifest`] mapping symbolic names → server
//!    specs (`command`, `args`, `extensions`, `root_markers`, an optional
//!    `install` shell recipe, etc.).
//! 2. On first use, [`install`] tries `which::which(&spec.command)`; falls
//!    back to a cached binary at
//!    `$XDG_DATA_HOME/rizz/lsp/<name>/bin/<command>`; finally runs the
//!    `install` recipe under `sh -c` with `RIZZ_LSP_DIR` pointing at the
//!    server's cache dir.
//! 3. [`try_load_cached`] is the pure-lookup variant used by the auto-attach
//!    hook so a buffer open doesn't block on a network round-trip.

pub mod cache;
mod error;
mod install;
mod manifest;

pub use cache::cache_root;
pub use error::InstallError;
pub use install::{install, try_load_cached, InstallOpts, InstalledServer};
pub use manifest::{Manifest, ServerSpec};
