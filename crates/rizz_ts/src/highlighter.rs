use crate::TsGrammar;
use std::rc::Rc;
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator, Tree};

/// One captured byte range with the conventional capture name (`"keyword"`,
/// `"string"`, …). Capture name comes from the live `Query` held by the
/// [`Highlighter`].
#[derive(Debug, Clone)]
pub struct HighlightSpan {
    pub start_byte: usize,
    pub end_byte: usize,
    pub capture: Rc<str>,
}

/// Per-buffer parser + query state. The `Rc<Query>` and `Rc<[Rc<str>]>` are
/// shared across clones (and across every highlighter built from the same
/// `Grammar`) so we don't rebuild them per buffer copy.
pub struct Highlighter {
    /// Kept so `Clone` can rebuild the (non-`Clone`) `Parser`, and so the
    /// underlying library lives at least as long as the highlighter.
    grammar: Rc<TsGrammar>,
    parser: Parser,
    query: Rc<Query>,
    /// Names of the query's captures, indexed by capture id. Shared with the
    /// `query`.
    capture_names: Rc<[Rc<str>]>,
    /// Snapshot of the text the current `tree` was parsed against. Held so
    /// `query` runs against the exact bytes the tree's nodes reference.
    text: String,
    tree: Option<Tree>,
    /// `true` once `set_source` runs and we haven't reparsed since.
    dirty: bool,
}

impl Highlighter {
    /// Build a highlighter from a registered [`TsGrammar`]. Infallible: the
    /// grammar's ABI was already vetted by [`TsRegistry::register`](registry::TsRegistry::register).
    pub fn new(grammar: Rc<TsGrammar>) -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&grammar.language)
            .expect("grammar ABI was vetted at registration");
        let query = grammar.query.clone();
        let capture_names = grammar.capture_names.clone();
        Self {
            grammar,
            parser,
            query,
            capture_names,
            text: String::new(),
            tree: None,
            dirty: true,
        }
    }

    /// Human-readable grammar name — the identifier passed at registration
    /// time.
    pub fn name(&self) -> &str {
        &self.grammar.name
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
        // `Parser` isn't `Clone`; rebuild from the recorded grammar. The
        // language pointer is stable across clones because `Rc<Grammar>`
        // keeps the underlying library alive.
        let mut parser = Parser::new();
        parser
            .set_language(&self.grammar.language)
            .expect("language ABI matches");
        Self {
            grammar: self.grammar.clone(),
            parser,
            query: self.query.clone(),
            capture_names: self.capture_names.clone(),
            text: self.text.clone(),
            tree: self.tree.clone(),
            dirty: self.dirty,
        }
    }
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
