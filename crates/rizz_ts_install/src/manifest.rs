//! Tree-sitter grammar spec. Maps a symbolic name (`"rust"`) to a repo URL
//! plus per-grammar quirks needed to find `parser.c` and `queries/highlights.scm`.
//!
//! Lives on disk as TOML next to `init.rz`. The editor seeds a bundled copy
//! on first launch; the user is free to add or override entries.
//!
//! The actual `Manifest` type (parsing, ext index, get/insert) lives in
//! `rizz_install::Manifest<S>`. This file just defines the spec and the
//! type alias.

use rizz_install::Spec;

/// One row from `grammars.toml`. Every field except `repo` and `extensions` is
/// optional — defaults are picked at install time.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct GrammarSpec {
    pub repo: String,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub rev: Option<String>,
    #[serde(default)]
    pub subdir: Option<String>,
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Override the C symbol suffix (`tree_sitter_<language>`) when the
    /// on-disk symbol doesn't match the table key. Defaults to the key.
    #[serde(default)]
    pub language: Option<String>,
    /// Override the highlights.scm path, relative to the source root. Defaults
    /// to `queries/highlights.scm` (or `<subdir>/queries/highlights.scm`).
    #[serde(default)]
    pub queries: Option<String>,
}

impl Spec for GrammarSpec {
    fn extensions(&self) -> &[String] {
        &self.extensions
    }
}

/// Parsed grammars manifest plus a reverse-index from file extension to
/// grammar name. A re-export of [`rizz_install::Manifest`] specialised for
/// [`GrammarSpec`].
pub type Manifest = rizz_install::Manifest<GrammarSpec>;
