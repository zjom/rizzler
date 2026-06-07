//! Tree-sitter-backed syntax highlighting for the rizz editor.
//!
//! [`Language`] enumerates the languages we have grammars for; [`Highlighter`]
//! owns a parser + query cache and a snapshot of the source it parsed against.
//! Callers feed text in via [`Highlighter::set_source`] (which marks the tree
//! dirty), then call [`Highlighter::highlights`] to iterate styled captures
//! clipped to a byte range — typically the visible viewport.
//!
//! Capture names are the conventional `nvim-treesitter` shorthand (`keyword`,
//! `string`, `function`, …); the renderer maps them to face names by
//! prepending `"syntax."` (see `rizz_ui::precompute`).

use std::path::Path;
use std::rc::Rc;

use tree_sitter::{Language as TsLanguage, Parser, Query, QueryCursor, StreamingIterator, Tree};

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

/// One captured byte range with the conventional capture name (`"keyword"`,
/// `"string"`, …). Lifetime is `'static` because capture names come from the
/// `Query`, which is held by the [`Highlighter`] for the duration.
#[derive(Debug, Clone)]
pub struct HighlightSpan {
    pub start_byte: usize,
    pub end_byte: usize,
    pub capture: Rc<str>,
}

/// Per-buffer parser + query state. The `Rc<Query>` and `Rc<[Rc<str>]>` are
/// shared across clones so we don't rebuild them per buffer copy.
pub struct Highlighter {
    pub lang: Language,
    parser: Parser,
    query: Rc<Query>,
    /// Names of the query's captures, indexed by capture id. Shared across
    /// clones.
    capture_names: Rc<[Rc<str>]>,
    /// Snapshot of the text the current `tree` was parsed against. Held so
    /// `highlights` can run queries against the exact bytes the tree's nodes
    /// reference.
    source: String,
    tree: Option<Tree>,
    /// `true` once `set_source` runs and we haven't reparsed since.
    dirty: bool,
}

impl Highlighter {
    pub fn new(lang: Language) -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&lang.ts_language())
            .expect("language ABI matches");
        let query = Query::new(&lang.ts_language(), lang.highlights_query())
            .expect("bundled highlights query is valid");
        let capture_names: Rc<[Rc<str>]> = query
            .capture_names()
            .iter()
            .map(|n| Rc::<str>::from(*n))
            .collect::<Vec<_>>()
            .into();
        Self {
            lang,
            parser,
            query: Rc::new(query),
            capture_names,
            source: String::new(),
            tree: None,
            dirty: true,
        }
    }

    /// Replace the snapshot text and mark the tree dirty. The next
    /// [`Self::ensure_parsed`] call will reparse against the new bytes.
    pub fn set_source(&mut self, src: String) {
        self.source = src;
        self.dirty = true;
    }

    /// Mark the tree dirty without replacing the source. Callers that don't
    /// have a fresh snapshot handy can use this to force a re-parse on next
    /// refresh; pair with [`Self::set_source`] before [`Self::ensure_parsed`].
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Reparse if needed. Reuses the previous tree as a hint when present —
    /// tree-sitter's incremental parse is efficient even without explicit edit
    /// deltas because it can reuse unchanged subtrees by content equality.
    pub fn ensure_parsed(&mut self) {
        if !self.dirty {
            return;
        }
        let old = self.tree.take();
        self.tree = self.parser.parse(&self.source, old.as_ref());
        self.dirty = false;
    }

    /// Iterate highlight captures whose byte range overlaps `[start_byte,
    /// end_byte)`. Read-only — caller must have parsed (via
    /// [`Self::ensure_parsed`]) since the last edit. Returns an empty `Vec`
    /// when no tree is available.
    pub fn query(&self, start_byte: usize, end_byte: usize) -> Vec<HighlightSpan> {
        let Some(tree) = self.tree.as_ref() else {
            return Vec::new();
        };
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(start_byte..end_byte);
        let mut out = Vec::new();
        let mut iter = cursor.matches(&self.query, tree.root_node(), self.source.as_bytes());
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
        let mut parser = Parser::new();
        parser
            .set_language(&self.lang.ts_language())
            .expect("language ABI matches");
        Self {
            lang: self.lang,
            parser,
            query: self.query.clone(),
            capture_names: self.capture_names.clone(),
            source: self.source.clone(),
            tree: self.tree.clone(),
            dirty: self.dirty,
        }
    }
}

impl std::fmt::Debug for Highlighter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Highlighter")
            .field("lang", &self.lang)
            .field("source_len", &self.source.len())
            .field("has_tree", &self.tree.is_some())
            .field("dirty", &self.dirty)
            .finish()
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
}
