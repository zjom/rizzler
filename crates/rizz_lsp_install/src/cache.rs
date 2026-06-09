//! Filesystem layout for cached LSP server binaries.
//!
//! ```text
//! $XDG_DATA_HOME/rizz/lsp/
//! └── <name>/
//!     ├── bin/<command>     binary produced by the install recipe
//!     ├── .stamp            sha256(recipe) + command, used to skip re-runs
//!     └── log/install.log   captured stdout+stderr from the last recipe run
//! ```

use std::path::{Path, PathBuf};

/// Root cache directory: `$XDG_DATA_HOME/rizz/lsp` (or
/// `$HOME/.local/share/rizz/lsp`).
pub fn cache_root() -> PathBuf {
    rizz_install::cache_root_for("lsp")
}

pub fn server_dir(root: &Path, name: &str) -> PathBuf {
    rizz_install::entry_dir(root, name)
}

pub fn bin_dir(root: &Path, name: &str) -> PathBuf {
    server_dir(root, name).join("bin")
}

pub fn binary_path(root: &Path, name: &str, command: &str) -> PathBuf {
    bin_dir(root, name).join(command)
}

pub fn stamp_path(root: &Path, name: &str) -> PathBuf {
    rizz_install::stamp_path(root, name)
}

pub fn log_path(root: &Path, name: &str) -> PathBuf {
    server_dir(root, name).join("log").join("install.log")
}
