//! Filesystem layout for cached grammar artefacts. Both [`install`](crate::install)
//! (writes) and [`try_load_cached`](crate::install::try_load_cached) (reads)
//! go through the helpers here so the path scheme has one source of truth.
//!
//! ```text
//! $XDG_DATA_HOME/rizz/grammars/
//! └── <name>/
//!     ├── parser.{so,dylib,dll}   the shared library libloading dlopens
//!     ├── highlights.scm          query, copied from the source repo
//!     ├── .stamp                  resolved repo+rev, used to skip rebuilds
//!     └── src/                    git checkout, kept for incremental rebuilds
//! ```

use std::path::{Path, PathBuf};

/// Root cache directory: `$XDG_DATA_HOME/rizz/grammars` (or
/// `$HOME/.local/share/rizz/grammars`).
pub fn cache_root() -> PathBuf {
    rizz_install::cache_root_for("grammars")
}

pub fn grammar_dir(root: &Path, name: &str) -> PathBuf {
    rizz_install::entry_dir(root, name)
}

pub fn source_dir(root: &Path, name: &str) -> PathBuf {
    grammar_dir(root, name).join("src")
}

pub fn library_path(root: &Path, name: &str) -> PathBuf {
    grammar_dir(root, name).join(library_filename())
}

pub fn highlights_path(root: &Path, name: &str) -> PathBuf {
    grammar_dir(root, name).join("highlights.scm")
}

pub fn stamp_path(root: &Path, name: &str) -> PathBuf {
    rizz_install::stamp_path(root, name)
}

/// Host-specific shared library filename. `libloading::Library::new` does no
/// auto-suffixing, so we pick the name explicitly and pass it through to
/// `tree-sitter build -o`.
pub const fn library_filename() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "parser.dylib"
    }
    #[cfg(target_os = "windows")]
    {
        "parser.dll"
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        "parser.so"
    }
}

/// Read-only snapshot of what's currently on disk for a grammar.
#[derive(Debug, Clone)]
pub struct CachedGrammar {
    pub library: PathBuf,
    pub highlights: PathBuf,
    pub stamp: Option<String>,
}

impl CachedGrammar {
    /// Return the cached artefacts for `name` only when both the library and
    /// the highlights file are present; a missing piece means the last install
    /// was partial and the caller should re-install.
    pub fn read(root: &Path, name: &str) -> Option<Self> {
        let library = library_path(root, name);
        let highlights = highlights_path(root, name);
        if !library.exists() || !highlights.exists() {
            return None;
        }
        let stamp = std::fs::read_to_string(stamp_path(root, name)).ok();
        Some(Self {
            library,
            highlights,
            stamp,
        })
    }
}
