//! Curated grammar manifest. Maps a symbolic name (`"rust"`) to a repo URL
//! plus the per-grammar quirks needed to find `parser.c` and
//! `queries/highlights.scm` inside it.
//!
//! Lives on disk as TOML next to `init.rz`. The editor seeds a bundled copy
//! on first launch; the user is free to add or override entries.

use std::collections::HashMap;

use crate::InstallError;

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

/// Parsed manifest plus a reverse-index from file extension to grammar name,
/// used by the auto-load pass.
#[derive(Debug, Default, Clone)]
pub struct Manifest {
    entries: HashMap<String, GrammarSpec>,
    by_ext: HashMap<String, String>,
}

impl Manifest {
    pub fn parse(toml_text: &str) -> Result<Self, InstallError> {
        let entries: HashMap<String, GrammarSpec> =
            toml::from_str(toml_text).map_err(|e| InstallError::Manifest {
                path: std::path::PathBuf::from("<manifest>"),
                source: e,
            })?;
        let mut by_ext = HashMap::new();
        for (name, spec) in &entries {
            for ext in &spec.extensions {
                let normalized = ext.trim_start_matches('.').to_ascii_lowercase();
                by_ext.entry(normalized).or_insert_with(|| name.clone());
            }
        }
        Ok(Self { entries, by_ext })
    }

    pub fn get(&self, name: &str) -> Option<&GrammarSpec> {
        self.entries.get(name)
    }

    /// Reverse-index lookup: `"rs"` → `"rust"`. Multiple grammars claiming the
    /// same extension resolve to whichever was inserted first.
    pub fn grammar_for_ext(&self, ext: &str) -> Option<&str> {
        let normalized = ext.trim_start_matches('.').to_ascii_lowercase();
        self.by_ext.get(&normalized).map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_indexes_extensions() {
        let m = Manifest::parse(
            r#"
[rust]
repo = "https://example.com/rust"
extensions = ["rs"]

[typescript]
repo = "https://example.com/typescript"
subdir = "typescript"
extensions = ["ts", "tsx"]
language = "typescript"
"#,
        )
        .unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m.grammar_for_ext("rs"), Some("rust"));
        assert_eq!(m.grammar_for_ext(".RS"), Some("rust"));
        assert_eq!(m.grammar_for_ext("ts"), Some("typescript"));
        assert_eq!(m.grammar_for_ext("nope"), None);
        let ts = m.get("typescript").unwrap();
        assert_eq!(ts.subdir.as_deref(), Some("typescript"));
        assert_eq!(ts.language.as_deref(), Some("typescript"));
    }

    #[test]
    fn empty_manifest_parses() {
        let m = Manifest::parse("").unwrap();
        assert!(m.is_empty());
        assert_eq!(m.grammar_for_ext("rs"), None);
    }

    #[test]
    fn bad_toml_errors() {
        let err = Manifest::parse("this is not = toml = at all").unwrap_err();
        assert!(matches!(err, InstallError::Manifest { .. }));
    }
}
