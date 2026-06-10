use crate::TsGrammar;
use ropey::Rope;
use std::rc::Rc;
use tree_sitter::{
    InputEdit, Node, Parser, Point, Query, QueryCursor, StreamingIterator, TextProvider, Tree,
};

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
///
/// Text is never snapshotted: parsing and querying both stream the caller's
/// rope chunk-by-chunk, so an edit in a large file doesn't pay an O(file)
/// copy before the (incremental) reparse.
pub struct Highlighter {
    /// Kept so `Clone` can rebuild the (non-`Clone`) `Parser`, and so the
    /// underlying library lives at least as long as the highlighter.
    grammar: Rc<TsGrammar>,
    parser: Parser,
    query: Rc<Query>,
    capture_names: Rc<[Rc<str>]>,
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
            tree: None,
            dirty: true,
        }
    }

    pub fn name(&self) -> &str {
        &self.grammar.name
    }

    /// Drop any cached tree and force a full reparse on the next
    /// [`Self::parse_rope`]. Used when the rope is replaced wholesale —
    /// incremental reuse only makes sense when the caller can describe every
    /// byte that moved.
    pub fn invalidate(&mut self) {
        self.tree = None;
        self.dirty = true;
    }

    /// Apply an incremental rope edit to the cached tree so the next
    /// [`Self::parse_rope`] can reuse unaffected subtrees. Coordinates are
    /// in tree-sitter's space (byte offsets + row/column-in-bytes) and must
    /// match the bytes the rope will hold at the next parse.
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

    /// Reparse `rope` if needed, streaming its chunks into tree-sitter and
    /// reusing the cached (edited) tree when possible. The cached tree is
    /// only safe to pass when every intervening edit has been fed through
    /// [`Self::record_edit`]; [`Self::invalidate`] clears it for the cases
    /// where that contract can't be upheld.
    pub fn parse_rope(&mut self, rope: &Rope) {
        if !self.dirty {
            return;
        }
        let mut chunk_at = |byte: usize, _pos: Point| -> &[u8] {
            if byte >= rope.len_bytes() {
                return &[];
            }
            let (chunk, chunk_start, _, _) = rope.chunk_at_byte(byte);
            &chunk.as_bytes()[byte - chunk_start..]
        };
        self.tree = self
            .parser
            .parse_with_options(&mut chunk_at, self.tree.as_ref(), None);
        self.dirty = false;
    }

    /// Iterate highlight captures whose byte range overlaps `[start_byte,
    /// end_byte)`. `rope` must be the text the current tree was parsed from
    /// (via [`Self::parse_rope`] since the last edit) — query predicates
    /// like `#match?` read node text out of it.
    pub fn query(&self, rope: &Rope, start_byte: usize, end_byte: usize) -> Vec<HighlightSpan> {
        let Some(tree) = self.tree.as_ref() else {
            return Vec::new();
        };
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(start_byte..end_byte);
        let mut out = Vec::new();
        let mut iter = cursor.matches(&self.query, tree.root_node(), RopeProvider(rope));
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

/// Zero-copy [`TextProvider`] over a rope: yields the chunks overlapping a
/// node's byte range.
struct RopeProvider<'a>(&'a Rope);

impl<'a> TextProvider<&'a [u8]> for RopeProvider<'a> {
    type I = std::iter::Map<ropey::iter::Chunks<'a>, fn(&str) -> &[u8]>;

    fn text(&mut self, node: Node) -> Self::I {
        let len = self.0.len_bytes();
        let start = node.start_byte().min(len);
        let end = node.end_byte().min(len);
        self.0
            .byte_slice(start..end)
            .chunks()
            .map(str::as_bytes as fn(&str) -> &[u8])
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
            tree: self.tree.clone(),
            dirty: self.dirty,
        }
    }
}

impl std::fmt::Debug for Highlighter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Highlighter")
            .field("source", &self.name())
            .field("has_tree", &self.tree.is_some())
            .field("dirty", &self.dirty)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TsRegistry;
    use std::path::{Path, PathBuf};

    /// Grammars are runtime-loaded shared libraries, so parse tests can only
    /// run where one is installed (`$XDG_DATA_HOME/rizz/grammars/<name>`).
    /// Returns `None` — and the caller skips — when it isn't.
    fn installed_grammar(name: &str, ext: &str) -> Option<Highlighter> {
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share"))
            })?;
        let dir = base.join("rizz").join("grammars").join(name);
        let lib = dir.join(if cfg!(target_os = "macos") {
            "parser.dylib"
        } else {
            "parser.so"
        });
        let query = std::fs::read_to_string(dir.join("highlights.scm")).ok()?;
        let mut reg = TsRegistry::new();
        reg.register(name, &[ext.to_string()], &lib, &query).ok()?;
        reg.highlighter_for_path(Path::new("test.rs"))
    }

    fn span_tuples(spans: &[HighlightSpan]) -> Vec<(usize, usize, Rc<str>)> {
        spans
            .iter()
            .map(|s| (s.start_byte, s.end_byte, s.capture.clone()))
            .collect()
    }

    /// Chunk-streamed parsing must produce the same captures as parsing the
    /// same text from one contiguous string.
    #[test]
    fn parse_rope_matches_whole_string_parse() {
        let Some(mut h) = installed_grammar("rust", "rs") else {
            eprintln!("skipping: rust grammar not installed locally");
            return;
        };
        let mut src = String::new();
        for i in 0..300 {
            src.push_str(&format!(
                "fn func_{i}() -> u64 {{ \"literal {i}\".len() as u64 }}\n"
            ));
        }
        let rope = Rope::from_str(&src);
        assert!(
            rope.chunks().count() > 1,
            "fixture must span multiple rope chunks"
        );

        h.parse_rope(&rope);
        let streamed = h.query(&rope, 0, rope.len_bytes());
        assert!(!streamed.is_empty(), "expected captures from the grammar");

        let mut whole = installed_grammar("rust", "rs").unwrap();
        whole.tree = whole.parser.parse(&src, None);
        whole.dirty = false;
        let contiguous = whole.query(&rope, 0, rope.len_bytes());

        assert_eq!(span_tuples(&streamed), span_tuples(&contiguous));
    }

    /// Timing probe: incremental reparse + viewport query cost per
    /// single-char edit in a 20k-line buffer. Run manually with
    /// `cargo test -p rizz_ts --release -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn bench_incremental_reparse() {
        let Some(mut h) = installed_grammar("rust", "rs") else {
            eprintln!("skipping: rust grammar not installed locally");
            return;
        };
        let mut src = String::new();
        for i in 0..20_000 {
            src.push_str(&format!(
                "fn func_{i}(x: u64) -> u64 {{ x.wrapping_mul({i}) + \"lit\".len() as u64 }}\n"
            ));
        }
        let mut rope = Rope::from_str(&src);
        let t = std::time::Instant::now();
        h.parse_rope(&rope);
        println!("initial parse: {:?}", t.elapsed());

        let line_start = rope.line_to_byte(10_000);
        let at = line_start;
        let t = std::time::Instant::now();
        let n = 200;
        let typed: Vec<char> = "let value = compute_something(input, flags); "
            .chars()
            .cycle()
            .take(n)
            .collect();
        for (i, c) in typed.into_iter().enumerate() {
            let byte = at + i;
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            rope.insert(rope.byte_to_char(byte), s);
            h.record_edit(
                byte,
                byte,
                byte + s.len(),
                Point { row: 10_000, column: byte - line_start },
                Point { row: 10_000, column: byte - line_start },
                Point { row: 10_000, column: byte - line_start + s.len() },
            );
            h.parse_rope(&rope);
            let _ = h.query(&rope, line_start, line_start + 4000);
        }
        println!("incremental parse+query: {:?}/edit", t.elapsed() / n as u32);
    }

    /// An incremental edit fed through `record_edit` + `parse_rope` must
    /// yield the same captures as a from-scratch parse of the edited text.
    #[test]
    fn incremental_reparse_matches_full_parse() {
        let Some(mut h) = installed_grammar("rust", "rs") else {
            eprintln!("skipping: rust grammar not installed locally");
            return;
        };
        let src = "fn main() {}\nfn other() -> u32 { 7 }\n";
        let mut rope = Rope::from_str(src);
        h.parse_rope(&rope);

        // Insert `let x = "s"; ` inside main's body (byte 11, row 0).
        let inserted = "let x = \"s\"; ";
        let at_byte = 11;
        rope.insert(rope.byte_to_char(at_byte), inserted);
        h.record_edit(
            at_byte,
            at_byte,
            at_byte + inserted.len(),
            Point { row: 0, column: at_byte },
            Point { row: 0, column: at_byte },
            Point {
                row: 0,
                column: at_byte + inserted.len(),
            },
        );
        h.parse_rope(&rope);
        let incremental = h.query(&rope, 0, rope.len_bytes());

        let mut fresh = installed_grammar("rust", "rs").unwrap();
        fresh.parse_rope(&rope);
        let full = fresh.query(&rope, 0, rope.len_bytes());

        assert_eq!(span_tuples(&incremental), span_tuples(&full));
        assert!(
            incremental
                .iter()
                .any(|s| rope.byte_slice(s.start_byte..s.end_byte).to_string() == "\"s\""),
            "the inserted string literal must be captured"
        );
    }
}
