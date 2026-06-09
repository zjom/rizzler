//! LSP server spec. Lives on disk as TOML next to `init.rz`. The editor
//! seeds a bundled copy on first launch; the user is free to add or
//! override entries via lisp (`lsp-register`, `lsp-install`).
//!
//! The actual `Manifest` type (parsing, ext index, get/insert) lives in
//! `rizz_install::Manifest<S>`. This file just defines the spec and the
//! type alias.

use std::collections::HashMap;

use rizz_install::Spec;

/// One row from `lsp.toml`. `command` is the only mandatory field.
#[derive(Debug, Clone, serde::Deserialize, Default)]
pub struct ServerSpec {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Extensions (without leading dot) the auto-load hook uses to pick
    /// this server when a buffer is opened.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Filenames whose presence (walking upward from the buffer's path)
    /// marks the workspace root. Empty falls back to the buffer's parent
    /// directory.
    #[serde(default)]
    pub root_markers: Vec<String>,
    /// Passed verbatim as the `initialize` request's `initializationOptions`.
    /// Stored as `toml::Value` so we can convert to JSON without an extra
    /// schema.
    #[serde(default)]
    pub initialization_options: Option<toml::Value>,
    /// Shell recipe run inside the per-server cache dir when the binary
    /// is missing from PATH. `RIZZ_LSP_DIR` points at the cache dir; the
    /// recipe must drop an executable at `$RIZZ_LSP_DIR/bin/<command>`.
    #[serde(default)]
    pub install: Option<String>,
    /// Extra env vars forwarded to the spawned server.
    #[serde(default)]
    pub env: HashMap<String, String>,
}

impl Spec for ServerSpec {
    fn extensions(&self) -> &[String] {
        &self.extensions
    }
}

/// Parsed LSP manifest plus a precomputed extension → server-name reverse
/// index. A re-export of [`rizz_install::Manifest`] specialised for
/// [`ServerSpec`].
pub type Manifest = rizz_install::Manifest<ServerSpec>;
