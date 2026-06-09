//! Curated LSP server manifest.
//!
//! Lives on disk as TOML next to `init.rz`. The editor seeds a bundled copy
//! on first launch; the user is free to add or override entries via lisp
//! (`lsp-register`, `lsp-install`).

use std::collections::HashMap;

use crate::InstallError;

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

/// Parsed manifest plus a precomputed extension → server-name reverse
/// index used by the auto-load pass.
#[derive(Debug, Default, Clone)]
pub struct Manifest {
    entries: HashMap<String, ServerSpec>,
    by_ext: HashMap<String, String>,
}

impl Manifest {
    pub fn parse(toml_text: &str) -> Result<Self, InstallError> {
        let entries: HashMap<String, ServerSpec> =
            toml::from_str(toml_text).map_err(|e| InstallError::Manifest {
                path: std::path::PathBuf::from("<manifest>"),
                source: e,
            })?;
        Ok(Self::from_entries(entries))
    }

    pub fn from_entries(entries: HashMap<String, ServerSpec>) -> Self {
        let mut by_ext = HashMap::new();
        for (name, spec) in &entries {
            for ext in &spec.extensions {
                let normalized = ext.trim_start_matches('.').to_ascii_lowercase();
                by_ext.entry(normalized).or_insert_with(|| name.clone());
            }
        }
        Self { entries, by_ext }
    }

    pub fn get(&self, name: &str) -> Option<&ServerSpec> {
        self.entries.get(name)
    }

    /// Reverse-index lookup: `"rs"` → `"rust-analyzer"`. Ties resolve to
    /// whichever server was inserted first.
    pub fn server_for_ext(&self, ext: &str) -> Option<&str> {
        let normalized = ext.trim_start_matches('.').to_ascii_lowercase();
        self.by_ext.get(&normalized).map(String::as_str)
    }

    /// Register or replace an entry. Backs `(lsp-register ...)` from lisp,
    /// for ad-hoc servers added without editing `lsp.toml`.
    pub fn insert(&mut self, name: String, spec: ServerSpec) {
        for ext in &spec.extensions {
            let normalized = ext.trim_start_matches('.').to_ascii_lowercase();
            self.by_ext.entry(normalized).or_insert_with(|| name.clone());
        }
        self.entries.insert(name, spec);
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &ServerSpec)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_indexes_extensions() {
        let m = Manifest::parse(
            r#"
[rust-analyzer]
command = "rust-analyzer"
extensions = ["rs"]
root_markers = ["Cargo.toml"]

[typescript-language-server]
command = "typescript-language-server"
args = ["--stdio"]
extensions = ["ts", "tsx"]
install = "npm install --prefix \"$RIZZ_LSP_DIR\" typescript-language-server typescript"
"#,
        )
        .unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m.server_for_ext("rs"), Some("rust-analyzer"));
        assert_eq!(m.server_for_ext(".TS"), Some("typescript-language-server"));
        assert_eq!(m.server_for_ext("nope"), None);
        let ts = m.get("typescript-language-server").unwrap();
        assert_eq!(ts.args, vec!["--stdio"]);
        assert!(ts.install.is_some());
    }

    #[test]
    fn empty_manifest_parses() {
        let m = Manifest::parse("").unwrap();
        assert!(m.is_empty());
    }

    #[test]
    fn bad_toml_errors() {
        let err = Manifest::parse("this is not = toml = at all").unwrap_err();
        assert!(matches!(err, InstallError::Manifest { .. }));
    }
}
