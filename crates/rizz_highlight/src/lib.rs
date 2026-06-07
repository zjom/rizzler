//! Tree-sitter-backed syntax highlighting for the rizz editor.
//!
//! Two grammar sources are supported:
//!
//! - **Native:** linked at compile time via the `tree-sitter-<lang>` crates.
//!   [`Language`] enumerates them and [`Highlighter::new`] is the constructor.
//! - **WASM:** loaded at runtime from a `.wasm` payload, gated behind the
//!   `wasm` feature (enabled by default). [`Highlighter::from_wasm`] builds
//!   one against a shared [`WasmEngine`] using a [`WasmGrammar`] descriptor
//!   (the parser bytes plus its `highlights.scm`).
//!
//! Both variants present the same surface: feed text in via
//! [`Highlighter::set_source`], call [`Highlighter::ensure_parsed`] to refresh
//! the tree, then [`Highlighter::query`] to iterate styled captures clipped to
//! a byte range — typically the visible viewport.
//!
//! Capture names follow the conventional `nvim-treesitter` shorthand
//! (`keyword`, `string`, `function`, …); the renderer maps them to face
//! names by prepending `"syntax."` (see `rizz_ui::precompute`).

use std::path::Path;
use std::rc::Rc;

use tree_sitter::{
    Language as TsLanguage, LanguageError, Parser, Query, QueryCursor, QueryError,
    StreamingIterator, Tree,
};

#[cfg(feature = "wasm")]
pub use wasm_support::{WasmEngine, WasmGrammar};

/// Built-in grammars compiled into the editor. Add a variant + the matching
/// arms in `ts_language` / `highlights_query` / `from_path` to support a new
/// native language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
}

impl Language {
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension().and_then(|e| e.to_str()) {
            Some("rs") => Some(Language::Rust),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Language::Rust => "rust",
        }
    }

    fn ts_language(self) -> TsLanguage {
        match self {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        }
    }

    fn highlights_query(self) -> &'static str {
        match self {
            Language::Rust => tree_sitter_rust::HIGHLIGHTS_QUERY,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HighlightError {
    #[error("invalid grammar ABI: {0}")]
    Abi(#[from] LanguageError),
    #[error("highlights query did not compile: {0}")]
    Query(#[from] QueryError),
    #[cfg(feature = "wasm")]
    #[error("wasm grammar load failed: {0}")]
    Wasm(#[from] tree_sitter::WasmError),
}

/// One captured byte range with the conventional capture name (`"keyword"`,
/// `"string"`, …). Capture name comes from the live `Query` held by the
/// [`Highlighter`].
#[derive(Debug, Clone)]
pub struct HighlightSpan {
    pub start_byte: usize,
    pub end_byte: usize,
    pub capture: Rc<str>,
}

/// What grammar a [`Highlighter`] was built against. Kept so `Clone` can
/// rebuild the parser/store from scratch (parsers and wasm stores are not
/// themselves cloneable).
#[derive(Clone)]
enum HighlightSource {
    Native(Language),
    #[cfg(feature = "wasm")]
    Wasm {
        engine: WasmEngine,
        grammar: Rc<WasmGrammar>,
    },
}

/// Per-buffer parser + query state. The `Rc<Query>` and `Rc<[Rc<str>]>` are
/// shared across clones so we don't rebuild them per buffer copy.
pub struct Highlighter {
    source: HighlightSource,
    parser: Parser,
    query: Rc<Query>,
    /// Names of the query's captures, indexed by capture id. Shared across
    /// clones.
    capture_names: Rc<[Rc<str>]>,
    /// Snapshot of the text the current `tree` was parsed against. Held so
    /// `query` runs against the exact bytes the tree's nodes reference.
    text: String,
    tree: Option<Tree>,
    /// `true` once `set_source` runs and we haven't reparsed since.
    dirty: bool,
}

impl Highlighter {
    /// Build a highlighter for a built-in [`Language`].
    pub fn new(lang: Language) -> Self {
        Self::native(lang).expect("bundled grammar + query are valid")
    }

    fn native(lang: Language) -> Result<Self, HighlightError> {
        let mut parser = Parser::new();
        parser.set_language(&lang.ts_language())?;
        let query = Query::new(&lang.ts_language(), lang.highlights_query())?;
        Ok(Self::wrap(HighlightSource::Native(lang), parser, query))
    }

    /// Build a highlighter from a `.wasm` parser payload + its highlights
    /// query. The `engine` is a shared `wasmtime::Engine` (cheap to clone via
    /// [`WasmEngine`]); each highlighter creates its own `WasmStore`.
    #[cfg(feature = "wasm")]
    pub fn from_wasm(
        engine: &WasmEngine,
        grammar: Rc<WasmGrammar>,
    ) -> Result<Self, HighlightError> {
        let (parser, query) = build_wasm_parser(engine, &grammar)?;
        Ok(Self::wrap(
            HighlightSource::Wasm {
                engine: engine.clone(),
                grammar,
            },
            parser,
            query,
        ))
    }

    fn wrap(source: HighlightSource, parser: Parser, query: Query) -> Self {
        let capture_names: Rc<[Rc<str>]> = query
            .capture_names()
            .iter()
            .map(|n| Rc::<str>::from(*n))
            .collect::<Vec<_>>()
            .into();
        Self {
            source,
            parser,
            query: Rc::new(query),
            capture_names,
            text: String::new(),
            tree: None,
            dirty: true,
        }
    }

    /// `Some` for native grammars, `None` for runtime-loaded WASM grammars
    /// (whose identity is just a string).
    pub fn language(&self) -> Option<Language> {
        match &self.source {
            HighlightSource::Native(l) => Some(*l),
            #[cfg(feature = "wasm")]
            HighlightSource::Wasm { .. } => None,
        }
    }

    /// Human-readable grammar name. For native grammars this is the enum
    /// variant's short name (`"rust"`); for WASM grammars it's the name the
    /// user supplied at registration time.
    pub fn name(&self) -> &str {
        match &self.source {
            HighlightSource::Native(l) => l.name(),
            #[cfg(feature = "wasm")]
            HighlightSource::Wasm { grammar, .. } => &grammar.name,
        }
    }

    /// Replace the snapshot text and mark the tree dirty. The next
    /// [`Self::ensure_parsed`] call will reparse against the new bytes.
    pub fn set_source(&mut self, src: String) {
        self.text = src;
        self.dirty = true;
    }

    /// Mark the tree dirty without replacing the source. Use to force a
    /// re-parse on next refresh; pair with [`Self::set_source`] before
    /// [`Self::ensure_parsed`].
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Reparse if needed. Reuses the previous tree as a hint when present.
    pub fn ensure_parsed(&mut self) {
        if !self.dirty {
            return;
        }
        let old = self.tree.take();
        self.tree = self.parser.parse(&self.text, old.as_ref());
        self.dirty = false;
    }

    /// Iterate highlight captures whose byte range overlaps `[start_byte,
    /// end_byte)`. Read-only — caller must have parsed (via
    /// [`Self::ensure_parsed`]) since the last edit.
    pub fn query(&self, start_byte: usize, end_byte: usize) -> Vec<HighlightSpan> {
        let Some(tree) = self.tree.as_ref() else {
            return Vec::new();
        };
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(start_byte..end_byte);
        let mut out = Vec::new();
        let mut iter = cursor.matches(&self.query, tree.root_node(), self.text.as_bytes());
        while let Some(m) = iter.next() {
            for cap in m.captures {
                let name = match self.capture_names.get(cap.index as usize) {
                    Some(n) => n.clone(),
                    None => continue,
                };
                let r = cap.node.byte_range();
                out.push(HighlightSpan {
                    start_byte: r.start,
                    end_byte: r.end,
                    capture: name,
                });
            }
        }
        out
    }
}

impl Clone for Highlighter {
    fn clone(&self) -> Self {
        // Parser and WasmStore are not cloneable; we rebuild them from the
        // recorded `HighlightSource`. Reusing the cached `Rc<Query>` keeps
        // this cheap on the native path; the wasm path rebuilds its store
        // (the engine clone is `Arc<EngineInner>` internally).
        let (parser, query_holder) = match &self.source {
            HighlightSource::Native(lang) => {
                let mut parser = Parser::new();
                parser
                    .set_language(&lang.ts_language())
                    .expect("language ABI matches");
                (parser, QueryHolder::Shared(self.query.clone()))
            }
            #[cfg(feature = "wasm")]
            HighlightSource::Wasm { engine, grammar } => {
                let (parser, query) =
                    build_wasm_parser(engine, grammar).expect("wasm grammar reload");
                (parser, QueryHolder::Fresh(Rc::new(query)))
            }
        };
        let (query, capture_names) = match query_holder {
            QueryHolder::Shared(q) => (q, self.capture_names.clone()),
            QueryHolder::Fresh(q) => {
                let names: Rc<[Rc<str>]> = q
                    .capture_names()
                    .iter()
                    .map(|n| Rc::<str>::from(*n))
                    .collect::<Vec<_>>()
                    .into();
                (q, names)
            }
        };
        Self {
            source: self.source.clone(),
            parser,
            query,
            capture_names,
            text: self.text.clone(),
            tree: self.tree.clone(),
            dirty: self.dirty,
        }
    }
}

/// Tiny helper that lets `clone` either reuse the existing `Rc<Query>`
/// (native) or take a freshly built one (wasm — the new `Query` is tied to
/// the new `WasmStore`'s `Language`, so it can't share with the original).
enum QueryHolder {
    Shared(Rc<Query>),
    #[cfg_attr(not(feature = "wasm"), allow(dead_code))]
    Fresh(Rc<Query>),
}

impl std::fmt::Debug for Highlighter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Highlighter")
            .field("source", &self.name())
            .field("text_len", &self.text.len())
            .field("has_tree", &self.tree.is_some())
            .field("dirty", &self.dirty)
            .finish()
    }
}

#[cfg(feature = "wasm")]
fn build_wasm_parser(
    engine: &WasmEngine,
    grammar: &WasmGrammar,
) -> Result<(Parser, Query), HighlightError> {
    let mut store = tree_sitter::WasmStore::new(engine.inner())?;
    let language = store.load_language(&grammar.name, &grammar.wasm_bytes)?;
    let mut parser = Parser::new();
    parser
        .set_wasm_store(store)
        .map_err(HighlightError::Abi)?;
    parser.set_language(&language)?;
    let query = Query::new(&language, &grammar.highlights_query)?;
    Ok((parser, query))
}

#[cfg(feature = "wasm")]
mod wasm_support {
    use std::rc::Rc;
    use tree_sitter::wasmtime;

    /// Shared `wasmtime::Engine`. Cloning is cheap (it's `Arc<EngineInner>`
    /// internally) so this can be handed to every [`super::Highlighter`].
    #[derive(Clone)]
    pub struct WasmEngine(wasmtime::Engine);

    impl WasmEngine {
        pub fn new() -> Self {
            Self(wasmtime::Engine::default())
        }

        pub(crate) fn inner(&self) -> &wasmtime::Engine {
            &self.0
        }
    }

    impl Default for WasmEngine {
        fn default() -> Self {
            Self::new()
        }
    }

    impl std::fmt::Debug for WasmEngine {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_tuple("WasmEngine").finish()
        }
    }

    /// A user-supplied tree-sitter grammar plus its highlights query. The
    /// fields are `Rc`'d so multiple buffers using the same language share
    /// storage. Build via [`super::Highlighter::from_wasm`].
    #[derive(Debug)]
    pub struct WasmGrammar {
        pub name: Rc<str>,
        pub wasm_bytes: Rc<[u8]>,
        pub highlights_query: Rc<str>,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn detects_rust_from_extension() {
        let p = PathBuf::from("foo.rs");
        assert_eq!(Language::from_path(&p), Some(Language::Rust));
    }

    #[test]
    fn rejects_unknown_extension() {
        let p = PathBuf::from("foo.txt");
        assert_eq!(Language::from_path(&p), None);
    }

    #[test]
    fn highlights_keyword_in_rust() {
        let mut h = Highlighter::new(Language::Rust);
        h.set_source("fn main() {}".to_string());
        h.ensure_parsed();
        let spans = h.query(0, 12);
        assert!(
            spans.iter().any(|s| &*s.capture == "keyword" && s.start_byte == 0 && s.end_byte == 2),
            "expected `fn` keyword capture, got {spans:?}"
        );
    }

    #[test]
    fn highlights_string_literal() {
        let mut h = Highlighter::new(Language::Rust);
        h.set_source(r#"fn x() { "hi"; }"#.to_string());
        h.ensure_parsed();
        let spans = h.query(0, 16);
        assert!(spans.iter().any(|s| &*s.capture == "string"));
    }

    #[test]
    fn native_highlighter_round_trips_through_clone() {
        let mut h = Highlighter::new(Language::Rust);
        h.set_source("fn x() {}".to_string());
        h.ensure_parsed();
        let mut clone = h.clone();
        // The clone should not need a fresh source — it carries the text.
        let spans = clone.query(0, 9);
        assert!(spans.iter().any(|s| &*s.capture == "keyword"));
        // Subsequent edits via the clone work.
        clone.set_source("fn y() {}".to_string());
        clone.ensure_parsed();
        assert!(clone.query(0, 9).iter().any(|s| &*s.capture == "keyword"));
    }
}
