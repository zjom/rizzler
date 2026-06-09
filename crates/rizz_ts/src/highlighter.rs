use crate::TsGrammar;
use std::rc::Rc;
use tree_sitter::{InputEdit, Parser, Point, Query, QueryCursor, StreamingIterator, Tree};

/// One captured byte range with the conventional capture name (`"keyword"`,
/// `"string"`, …).
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
    capture_names: Rc<[Rc<str>]>,
    /// Snapshot of the text the current `tree` was parsed against. Held so
    /// `query` runs against the exact bytes the tree's nodes reference.
    text: String,
    tree: Option<Tree>,
    dirty: bool,
}

impl Highlighter {
    /// Infallible: the grammar's ABI was already vetted by
    /// [`TsRegistry::register`](registry::TsRegistry::register).
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

    pub fn name(&self) -> &str {
        &self.grammar.name
    }

    /// Replace the snapshot text. The next [`Self::ensure_parsed`] call reuses
    /// the cached tree as a base, so the caller must have fed any intervening
    /// edits through [`Self::record_edit`] first.
    pub fn set_source(&mut self, src: String) {
        self.text = src;
        self.dirty = true;
    }

    /// Drop any cached tree and force a full reparse on the next
    /// [`Self::ensure_parsed`]. Used when the rope is replaced wholesale —
    /// incremental reuse only makes sense when the caller can describe every
    /// byte that moved.
    pub fn invalidate(&mut self) {
        self.tree = None;
        self.dirty = true;
    }

    /// Apply an incremental rope edit to the cached tree so the next
    /// [`Self::ensure_parsed`] can reuse unaffected subtrees. Coordinates are
    /// in tree-sitter's space (byte offsets + row/column-in-bytes) and must
    /// match the bytes that will be in the source on the next `set_source`.
    pub fn record_edit(
        &mut self,
        start_byte: usize,
        old_end_byte: usize,
        new_end_byte: usize,
        start_position: Point,
        old_end_position: Point,
        new_end_position: Point,
    ) {
        if let Some(tree) = self.tree.as_mut() {
            tree.edit(&InputEdit {
                start_byte,
                old_end_byte,
                new_end_byte,
                start_position,
                old_end_position,
                new_end_position,
            });
        }
        self.dirty = true;
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Reparse if needed, reusing the cached (edited) tree when possible. The
    /// cached tree is only safe to pass when every intervening edit has been
    /// fed through [`Self::record_edit`]; [`Self::invalidate`] clears it for
    /// the cases where that contract can't be upheld.
    pub fn ensure_parsed(&mut self) {
        if !self.dirty {
            return;
        }
        self.tree = self.parser.parse(&self.text, self.tree.as_ref());
        self.dirty = false;
    }

    /// Iterate highlight captures whose byte range overlaps `[start_byte,
    /// end_byte)`. Caller must have parsed (via [`Self::ensure_parsed`]) since
    /// the last edit.
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
        // `Parser` isn't `Clone`; rebuild from the recorded grammar.
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
