//! Filesystem layout for cached grammar artefacts.
//!
//! ```text
//! $XDG_DATA_HOME/rizz/grammars/
//! └── <name>/
//!     ├── parser.{so,dylib,dll}   the shared library libloading dlopens
//!     ├── highlights.scm          query, copied from the source repo
//!     ├── .stamp                  resolved repo+rev, used to skip rebuilds
//!     └── src/                    git checkout, kept for incremental rebuilds
//! ```
//!
//! Layout is shared between [`install`](crate::install) (writes) and
//! [`try_load_cached`](crate::install::try_load_cached) (reads) — both go
//! through the helpers here so the path scheme has exactly one source of
//! truth.

use std::path::{Path, PathBuf};

/// Root cache directory: `$XDG_DATA_HOME/rizz/grammars` (or
/// `$HOME/.local/share/rizz/grammars`).
pub fn cache_root() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("rizz").join("grammars")
}

pub fn grammar_dir(root: &Path, name: &str) -> PathBuf {
    root.join(name)
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
    grammar_dir(root, name).join(".stamp")
}

/// Host-specific shared library filename. `libloading::Library::new` does no
/// auto-suffixing, so we pick the right name explicitly and pass it through
/// to `tree-sitter build -o`.
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
    /// Return the cached artefacts for `name`, but only when both the library
    /// and the highlights file are on disk. Missing either piece means the
    /// last install was partial; callers should re-install.
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
