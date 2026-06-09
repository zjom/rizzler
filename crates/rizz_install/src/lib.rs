//! Shared primitives for declarative language-backend installers.
//!
//! Tree-sitter grammars and LSP servers both follow the same pattern:
//!
//! 1. A curated TOML manifest maps symbolic names to specs.
//! 2. Each spec lists file extensions; the auto-load hook reverses this
//!    into an `ext → name` index.
//! 3. The editor tracks per-name one-shot warnings (so opening many `.py`
//!    files doesn't spam) and failed auto-installs (so we don't retry).
//! 4. Cache artefacts land under `$XDG_DATA_HOME/rizz/<kind>/<name>/`.
//!
//! This crate factors out (1)–(4) so `rizz_ts_install` and `rizz_lsp_install`
//! only need to spell out the bits that differ — their concrete `Spec` type
//! and their concrete install side effects (git+tree-sitter vs. shell recipe).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// A manifest entry. Implementors describe one installable backend (a
/// tree-sitter grammar, an LSP server, …) plus the file extensions that
/// pick it.
pub trait Spec: serde::de::DeserializeOwned + Clone {
    /// Extensions (without leading dot) the auto-load hook uses to pick
    /// this backend when a buffer is opened.
    fn extensions(&self) -> &[String];
}

/// Parsed TOML manifest plus a precomputed extension → name reverse index.
/// Generic over the per-entry spec; concrete crates re-export with the
/// right `S` (`Manifest<GrammarSpec>`, `Manifest<ServerSpec>`).
#[derive(Debug, Clone)]
pub struct Manifest<S> {
    entries: HashMap<String, S>,
    by_ext: HashMap<String, String>,
}

impl<S> Default for Manifest<S> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            by_ext: HashMap::new(),
        }
    }
}

impl<S: Spec> Manifest<S> {
    /// Parse a TOML document of `[name] … extensions = […]` tables.
    pub fn parse(toml_text: &str) -> Result<Self, ManifestError> {
        let entries: HashMap<String, S> =
            toml::from_str(toml_text).map_err(|e| ManifestError::Parse {
                path: PathBuf::from("<manifest>"),
                source: e,
            })?;
        Ok(Self::from_entries(entries))
    }

    /// Build from an already-deserialised map. Used by `parse` and by
    /// runtime registration (`Manifest::insert`).
    pub fn from_entries(entries: HashMap<String, S>) -> Self {
        let mut by_ext = HashMap::new();
        for (name, spec) in &entries {
            for ext in spec.extensions() {
                let normalized = normalize_ext(ext);
                by_ext.entry(normalized).or_insert_with(|| name.clone());
            }
        }
        Self { entries, by_ext }
    }

    pub fn get(&self, name: &str) -> Option<&S> {
        self.entries.get(name)
    }

    /// Reverse-index lookup: `"rs"` → `"rust"`. Multiple entries claiming
    /// the same extension resolve to whichever was inserted first.
    pub fn lookup_by_ext(&self, ext: &str) -> Option<&str> {
        let normalized = normalize_ext(ext);
        self.by_ext.get(&normalized).map(String::as_str)
    }

    /// Register or replace an entry at runtime. Used by `(lsp-register …)`
    /// from lisp for ad-hoc backends added without editing the manifest.
    pub fn insert(&mut self, name: String, spec: S) {
        for ext in spec.extensions() {
            let normalized = normalize_ext(ext);
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

    pub fn iter(&self) -> impl Iterator<Item = (&str, &S)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }
}

fn normalize_ext(ext: &str) -> String {
    ext.trim_start_matches('.').to_ascii_lowercase()
}

/// Editor-side workflow state for one language backend: the manifest plus
/// the auto-install toggle and the per-name "already warned" / "already
/// failed auto-install" sets.
///
/// Both grammar and LSP integrations hold one of these. The shared shape
/// makes the install-highlighter / install-lsp-client flows in
/// `rizz_editor` map onto a single helper instead of mirroring each other.
pub struct LanguageBackend<S> {
    pub manifest: Manifest<S>,
    /// When true, opening a file whose extension matches an unbuilt entry
    /// triggers a one-shot install attempt.
    pub auto_install: bool,
    /// Names we've already surfaced a "not installed" notify for. Prevents
    /// per-buffer warning spam.
    pub warned_missing: HashSet<Rc<str>>,
    /// Names whose auto-install we already tried and which failed.
    /// Prevents retry-on-every-buffer-open.
    pub failed_auto_installs: HashSet<Rc<str>>,
}

impl<S> LanguageBackend<S> {
    pub fn new(manifest: Manifest<S>) -> Self {
        Self {
            manifest,
            auto_install: true,
            warned_missing: HashSet::new(),
            failed_auto_installs: HashSet::new(),
        }
    }

    /// Drop the one-shot trackers for `name`. Used after a successful
    /// manual install / `reload-config` so future opens can warn or retry.
    pub fn forget(&mut self, name: &str) {
        let key = Rc::<str>::from(name);
        self.warned_missing.remove(&key);
        self.failed_auto_installs.remove(&key);
    }

    /// Atomic "should I warn for this name?" — returns true exactly once
    /// per name (subsequent calls return false), and inserts on the way.
    pub fn first_warn(&mut self, name: &str) -> bool {
        self.warned_missing.insert(Rc::<str>::from(name))
    }

    pub fn mark_failed(&mut self, name: &str) {
        self.failed_auto_installs.insert(Rc::<str>::from(name));
    }

    pub fn already_failed(&self, name: &str) -> bool {
        let key: Rc<str> = Rc::from(name);
        self.failed_auto_installs.contains(&key)
    }
}

/// Root cache directory for a given backend kind:
/// `$XDG_DATA_HOME/rizz/<kind>` (or `~/.local/share/rizz/<kind>`).
pub fn cache_root_for(kind: &str) -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("rizz").join(kind)
}

/// Per-entry directory: `<root>/<name>`. Both install paths agree on this.
pub fn entry_dir(root: &Path, name: &str) -> PathBuf {
    root.join(name)
}

/// Stamp file used to skip rebuilds when the install inputs haven't
/// changed: `<root>/<name>/.stamp`.
pub fn stamp_path(root: &Path, name: &str) -> PathBuf {
    entry_dir(root, name).join(".stamp")
}

/// Errors surfaced when parsing a manifest. Concrete install crates wrap
/// this in their domain-specific error enum.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest at {path} could not be parsed: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, serde::Deserialize)]
    struct TestSpec {
        #[serde(default)]
        extensions: Vec<String>,
    }
    impl Spec for TestSpec {
        fn extensions(&self) -> &[String] {
            &self.extensions
        }
    }

    #[test]
    fn parses_and_indexes_extensions() {
        let m: Manifest<TestSpec> = Manifest::parse(
            r#"
[rust]
extensions = ["rs"]
[typescript]
extensions = ["ts", "TSX"]
"#,
        )
        .unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m.lookup_by_ext("rs"), Some("rust"));
        assert_eq!(m.lookup_by_ext(".RS"), Some("rust"));
        assert_eq!(m.lookup_by_ext("tsx"), Some("typescript"));
        assert_eq!(m.lookup_by_ext("nope"), None);
    }

    #[test]
    fn empty_manifest_parses() {
        let m: Manifest<TestSpec> = Manifest::parse("").unwrap();
        assert!(m.is_empty());
    }

    #[test]
    fn insert_updates_ext_index() {
        let mut m: Manifest<TestSpec> = Manifest::default();
        m.insert(
            "py".to_string(),
            TestSpec {
                extensions: vec!["py".to_string()],
            },
        );
        assert_eq!(m.lookup_by_ext("py"), Some("py"));
    }

    #[test]
    fn first_warn_returns_true_once() {
        let mut b: LanguageBackend<TestSpec> = LanguageBackend::new(Manifest::default());
        assert!(b.first_warn("rust"));
        assert!(!b.first_warn("rust"));
        b.forget("rust");
        assert!(b.first_warn("rust"));
    }

    #[test]
    fn mark_failed_round_trip() {
        let mut b: LanguageBackend<TestSpec> = LanguageBackend::new(Manifest::default());
        assert!(!b.already_failed("rust"));
        b.mark_failed("rust");
        assert!(b.already_failed("rust"));
        b.forget("rust");
        assert!(!b.already_failed("rust"));
    }
}
