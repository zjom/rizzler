//! Vim-style text objects (`iw`, `aw`, `i(`, `a"`, …).
//!
//! A text object resolves to a half-open char range the surrounding operator
//! (delete / yank / change / visual) then acts on. The two flavors:
//!
//! * **inner** (`i<x>`) — just the contents.
//! * **around** (`a<x>`) — contents plus their delimiters. Word objects also
//!   include the adjacent whitespace.
//!
//! Resolution is pure: [`Buffer::text_object_range`] consults the rope at
//! the cursor and returns `Option<(start, end, RegisterKind)>`. The state
//! layer wires it to actions that capture the spanned text into a register
//! and then mutate (or, for `v<x>`, set up a visual selection).

use std::str::FromStr;

use ropey::Rope;

use rizz_registers::RegisterKind;

use super::Buffer;

/// The set of text objects the editor knows how to resolve. The string
/// values in the [`FromStr`] impl are what the lisp `(delete-inner ...)` /
/// `(yank-around ...)` builtins accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextObject {
    /// Vim `iw` / `aw` — a word (alphanumeric + `_`) or a punctuation run.
    Word,
    /// Vim `iW` / `aW` — a "big word" (any non-whitespace run).
    BigWord,
    /// Vim `i(` / `a(` / `ib` / `ab` — parenthesized block.
    Paren,
    /// Vim `i[` / `a[` — bracketed block.
    Bracket,
    /// Vim `i{` / `a{` / `iB` / `aB` — braced block.
    Brace,
    /// Vim `i<` / `a<` — angle-bracketed block. Useful for generics/HTML.
    Angle,
    /// Vim `i"` / `a"` — double-quoted text on the current line.
    DoubleQuote,
    /// Vim `i'` / `a'` — single-quoted text on the current line.
    SingleQuote,
    /// Vim `` i` `` / `` a` `` — backtick-quoted text on the current line.
    Backtick,
}

impl FromStr for TextObject {
    type Err = &'static str;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "word" => TextObject::Word,
            "big-word" => TextObject::BigWord,
            "paren" | "parens" | "(" | ")" | "b" => TextObject::Paren,
            "bracket" | "brackets" | "[" | "]" => TextObject::Bracket,
            "brace" | "braces" | "{" | "}" | "B" => TextObject::Brace,
            "angle" | "angles" | "<" | ">" => TextObject::Angle,
            "double-quote" | "dquote" | "\"" => TextObject::DoubleQuote,
            "single-quote" | "squote" | "'" => TextObject::SingleQuote,
            "backtick" | "btick" | "`" => TextObject::Backtick,
            _ => return Err("unknown TextObject"),
        })
    }
}

impl Buffer {
    /// Resolve a text object at the cursor to a half-open char range.
    /// Returns `None` when no enclosing/at-cursor match exists (e.g. `i(`
    /// on a line with no parens). Always charwise — paragraph/sentence
    /// objects, which would be linewise, are not implemented.
    ///
    /// `count` is honored by the pair-style objects (expand outward N
    /// levels) and is ignored by word/quote objects.
    pub fn text_object_range(
        &self,
        obj: TextObject,
        around: bool,
        count: u32,
    ) -> Option<(usize, usize, RegisterKind)> {
        let cidx = self.cur_line_start() + self.file_pos.col + self.cursor_pos.col as usize;
        let count = count.max(1) as usize;
        let range = match obj {
            TextObject::Word => word_range(&self.buf, cidx, around, false),
            TextObject::BigWord => word_range(&self.buf, cidx, around, true),
            TextObject::Paren => pair_range(&self.buf, cidx, around, count, '(', ')'),
            TextObject::Bracket => pair_range(&self.buf, cidx, around, count, '[', ']'),
            TextObject::Brace => pair_range(&self.buf, cidx, around, count, '{', '}'),
            TextObject::Angle => pair_range(&self.buf, cidx, around, count, '<', '>'),
            TextObject::DoubleQuote => quote_range(&self.buf, cidx, around, '"'),
            TextObject::SingleQuote => quote_range(&self.buf, cidx, around, '\''),
            TextObject::Backtick => quote_range(&self.buf, cidx, around, '`'),
        };
        range.map(|(s, e)| (s, e, RegisterKind::Char))
    }
}

// ---- word --------------------------------------------------------------

#[derive(PartialEq, Eq, Copy, Clone, Debug)]
enum CharClass {
    Ws,
    Word,
    Punct,
}

fn classify(c: char, big: bool) -> CharClass {
    if c.is_whitespace() {
        CharClass::Ws
    } else if big || c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Punct
    }
}

/// Resolve `iw` / `aw` (or `iW` / `aW`). The "inner" form is the run of
/// same-class chars at the cursor. The "around" form extends that run with
/// trailing whitespace; if no trailing whitespace exists on the line, it
/// extends with leading whitespace instead (matches vim).
fn word_range(rope: &Rope, cidx: usize, around: bool, big: bool) -> Option<(usize, usize)> {
    let len = rope.len_chars();
    if len == 0 {
        return None;
    }
    let i = cidx.min(len.saturating_sub(1));
    let class = classify(rope.char(i), big);

    let (start, end) = same_class_span(rope, i, class, big);

    if !around {
        return Some((start, end));
    }

    // Around: include the trailing whitespace (or, if at end-of-line,
    // leading whitespace) when the object itself is a word/punct; if the
    // object IS whitespace, include the following word instead.
    if class == CharClass::Ws {
        // Whitespace object: extend with the following word run.
        let mut j = end;
        if j < len {
            let next_class = classify(rope.char(j), big);
            if next_class != CharClass::Ws {
                let (_, e) = same_class_span(rope, j, next_class, big);
                j = e;
            }
        }
        return Some((start, j));
    }

    // Word/punct object: try trailing whitespace first.
    let mut j = end;
    let mut extended_trailing = false;
    while j < len && rope.char(j) != '\n' && rope.char(j).is_whitespace() {
        j += 1;
        extended_trailing = true;
    }
    if extended_trailing {
        return Some((start, j));
    }

    // No trailing whitespace on the line — pull in leading whitespace.
    let mut s = start;
    while s > 0 {
        let prev = rope.char(s - 1);
        if !prev.is_whitespace() || prev == '\n' {
            break;
        }
        s -= 1;
    }
    Some((s, end))
}

/// Walk left and right from `i` while the char class matches, returning the
/// half-open `[start, end)` span. Newlines belong to the whitespace class
/// but also terminate a word run (so `w<newline>w` is two word objects, not
/// one).
fn same_class_span(rope: &Rope, i: usize, class: CharClass, big: bool) -> (usize, usize) {
    let len = rope.len_chars();
    let mut s = i;
    while s > 0 {
        let prev = rope.char(s - 1);
        if prev == '\n' || classify(prev, big) != class {
            break;
        }
        s -= 1;
    }
    let mut e = i + 1;
    while e < len {
        let c = rope.char(e);
        if c == '\n' || classify(c, big) != class {
            break;
        }
        e += 1;
    }
    (s, e)
}

// ---- pair (parens/brackets/braces/angles) -------------------------------

/// Resolve `i<pair>` / `a<pair>`. Walks left from the cursor with a depth
/// counter to find the nearest enclosing opener; then walks right from that
/// opener to find its matching closer. `count` expands outward by treating
/// the just-found opener as the cursor and repeating.
fn pair_range(
    rope: &Rope,
    cidx: usize,
    around: bool,
    count: usize,
    open: char,
    close: char,
) -> Option<(usize, usize)> {
    let len = rope.len_chars();
    if len == 0 {
        return None;
    }
    let mut cursor = cidx.min(len.saturating_sub(1));
    let mut open_at = None;
    let mut close_at = None;
    for _ in 0..count {
        let (o, c) = enclosing_pair(rope, cursor, open, close)?;
        open_at = Some(o);
        close_at = Some(c);
        if o == 0 {
            // can't expand further — stop here even if more counts requested
            break;
        }
        cursor = o.saturating_sub(1);
    }
    let o = open_at?;
    let c = close_at?;
    if around {
        Some((o, c + 1))
    } else {
        Some((o + 1, c))
    }
}

/// Find the innermost `(open, close)` pair that *encloses* `cidx`. If
/// `cidx` itself sits on `open`/`close`, that bracket is used as the
/// matching endpoint.
fn enclosing_pair(rope: &Rope, cidx: usize, open: char, close: char) -> Option<(usize, usize)> {
    let len = rope.len_chars();
    if cidx >= len {
        return None;
    }
    let here = rope.char(cidx);
    let (o, search_from_close) = if here == open {
        let c = match_forward(rope, cidx, open, close)?;
        return Some((cidx, c));
    } else if here == close {
        let o = match_backward(rope, cidx, open, close)?;
        return Some((o, cidx));
    } else {
        // Walk left to find an unmatched opener.
        let o = unmatched_opener(rope, cidx, open, close)?;
        (o, true)
    };
    let _ = search_from_close;
    let c = match_forward(rope, o, open, close)?;
    Some((o, c))
}

/// From `cidx` (which sits on `open`), find the matching `close` index.
fn match_forward(rope: &Rope, cidx: usize, open: char, close: char) -> Option<usize> {
    let len = rope.len_chars();
    let mut depth: usize = 1;
    let mut i = cidx + 1;
    while i < len {
        let c = rope.char(i);
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// From `cidx` (which sits on `close`), find the matching `open` index.
fn match_backward(rope: &Rope, cidx: usize, open: char, close: char) -> Option<usize> {
    if cidx == 0 {
        return None;
    }
    let mut depth: usize = 1;
    let mut i = cidx - 1;
    loop {
        let c = rope.char(i);
        if c == close {
            depth += 1;
        } else if c == open {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        if i == 0 {
            break;
        }
        i -= 1;
    }
    None
}

/// Walk left from `cidx` looking for an `open` whose matching `close` has
/// not yet been seen — i.e. the opener of the enclosing pair.
fn unmatched_opener(rope: &Rope, cidx: usize, open: char, close: char) -> Option<usize> {
    if cidx == 0 {
        return None;
    }
    let mut depth: usize = 0;
    let mut i = cidx;
    while i > 0 {
        i -= 1;
        let c = rope.char(i);
        if c == close {
            depth += 1;
        } else if c == open {
            if depth == 0 {
                return Some(i);
            }
            depth -= 1;
        }
    }
    None
}

// ---- quote --------------------------------------------------------------

/// Resolve `i"` / `a"` (or `'` / `` ` ``). Vim restricts quote objects to
/// the current line: scan the line's quote positions, pair them sequentially
/// (`[0,1] [2,3] ...`), and pick the pair whose range contains `cidx` — or,
/// if `cidx` sits in a between-pairs gap, the next pair to the right.
fn quote_range(rope: &Rope, cidx: usize, around: bool, q: char) -> Option<(usize, usize)> {
    let len = rope.len_chars();
    if len == 0 {
        return None;
    }
    let row = rope.char_to_line(cidx.min(len.saturating_sub(1)));
    let line_start = rope.line_to_char(row);
    let line = rope.line(row);
    let mut line_len = line.len_chars();
    if line_len > 0 && line.char(line_len - 1) == '\n' {
        line_len -= 1;
    }

    // Collect quote positions on this line. Skip escaped quotes (`\"`).
    let mut quotes: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < line_len {
        let c = line.char(i);
        if c == '\\' {
            i += 2;
            continue;
        }
        if c == q {
            quotes.push(line_start + i);
        }
        i += 1;
    }
    if quotes.len() < 2 {
        return None;
    }

    // Pair them: (quotes[0], quotes[1]), (quotes[2], quotes[3]), ...
    let mut chosen = None;
    let mut k = 0;
    while k + 1 < quotes.len() {
        let (o, c) = (quotes[k], quotes[k + 1]);
        if cidx >= o && cidx <= c {
            chosen = Some((o, c));
            break;
        }
        if cidx < o {
            chosen = Some((o, c));
            break;
        }
        k += 2;
    }
    let (o, c) = chosen?;

    if !around {
        return Some((o + 1, c));
    }

    // Around includes the quotes. Vim extends with trailing whitespace,
    // and if there's none, with leading whitespace.
    let mut end = c + 1;
    let mut extended = false;
    while end < line_start + line_len {
        let ch = rope.char(end);
        if !ch.is_whitespace() || ch == '\n' {
            break;
        }
        end += 1;
        extended = true;
    }
    if extended {
        return Some((o, end));
    }
    let mut start = o;
    while start > line_start {
        let prev = rope.char(start - 1);
        if !prev.is_whitespace() || prev == '\n' {
            break;
        }
        start -= 1;
    }
    Some((start, c + 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rizz_core::Position;
    use std::str::FromStr;

    fn mk(text: &str) -> Buffer {
        Buffer::from_str(text).unwrap()
    }

    impl Buffer {
        fn text_at(&self, lo: usize, hi: usize) -> String {
            self.buf.slice(lo..hi).to_string()
        }
    }

    // ---- word -----------------------------------------------------------

    #[test]
    fn iw_on_word_returns_word() {
        let mut s = mk("hello world");
        s.cursor_pos = Position::new(2, 0);
        let (lo, hi, _) = s.text_object_range(TextObject::Word, false, 1).unwrap();
        assert_eq!(&s.text_at(lo, hi), "hello");
    }

    #[test]
    fn aw_on_word_includes_trailing_space() {
        let mut s = mk("hello world");
        s.cursor_pos = Position::new(2, 0);
        let (lo, hi, _) = s.text_object_range(TextObject::Word, true, 1).unwrap();
        assert_eq!(&s.text_at(lo, hi), "hello ");
    }

    #[test]
    fn aw_at_end_of_line_includes_leading_space() {
        let mut s = mk("hello world");
        s.cursor_pos = Position::new(8, 0);
        let (lo, hi, _) = s.text_object_range(TextObject::Word, true, 1).unwrap();
        assert_eq!(&s.text_at(lo, hi), " world");
    }

    #[test]
    fn iw_on_whitespace_returns_whitespace_run() {
        let mut s = mk("a   b");
        s.cursor_pos = Position::new(2, 0);
        let (lo, hi, _) = s.text_object_range(TextObject::Word, false, 1).unwrap();
        assert_eq!(&s.text_at(lo, hi), "   ");
    }

    #[test]
    fn iw_treats_punctuation_as_own_class() {
        let mut s = mk("foo.bar");
        s.cursor_pos = Position::new(3, 0);
        let (lo, hi, _) = s.text_object_range(TextObject::Word, false, 1).unwrap();
        assert_eq!(&s.text_at(lo, hi), ".");
    }

    #[test]
    fn big_word_treats_punctuation_as_word_char() {
        let mut s = mk("foo.bar baz");
        s.cursor_pos = Position::new(3, 0);
        let (lo, hi, _) = s.text_object_range(TextObject::BigWord, false, 1).unwrap();
        assert_eq!(&s.text_at(lo, hi), "foo.bar");
    }

    // ---- pair -----------------------------------------------------------

    #[test]
    fn i_paren_excludes_brackets() {
        let mut s = mk("foo(bar baz)qux");
        s.cursor_pos = Position::new(5, 0);
        let (lo, hi, _) = s.text_object_range(TextObject::Paren, false, 1).unwrap();
        assert_eq!(&s.text_at(lo, hi), "bar baz");
    }

    #[test]
    fn a_paren_includes_brackets() {
        let mut s = mk("foo(bar baz)qux");
        s.cursor_pos = Position::new(5, 0);
        let (lo, hi, _) = s.text_object_range(TextObject::Paren, true, 1).unwrap();
        assert_eq!(&s.text_at(lo, hi), "(bar baz)");
    }

    #[test]
    fn i_paren_finds_innermost_when_nested() {
        let mut s = mk("a(b(c)d)e");
        s.cursor_pos = Position::new(4, 0);
        let (lo, hi, _) = s.text_object_range(TextObject::Paren, false, 1).unwrap();
        assert_eq!(&s.text_at(lo, hi), "c");
    }

    #[test]
    fn paren_count_expands_outward() {
        let mut s = mk("a(b(c)d)e");
        s.cursor_pos = Position::new(4, 0);
        let (lo, hi, _) = s.text_object_range(TextObject::Paren, false, 2).unwrap();
        assert_eq!(&s.text_at(lo, hi), "b(c)d");
    }

    #[test]
    fn i_paren_when_cursor_on_open_uses_that_pair() {
        let mut s = mk("(hello)");
        s.cursor_pos = Position::new(0, 0);
        let (lo, hi, _) = s.text_object_range(TextObject::Paren, false, 1).unwrap();
        assert_eq!(&s.text_at(lo, hi), "hello");
    }

    #[test]
    fn pair_returns_none_when_unmatched() {
        let mut s = mk("(foo");
        s.cursor_pos = Position::new(2, 0);
        // The open bracket has no closer — no enclosing pair to operate on.
        assert!(s.text_object_range(TextObject::Paren, true, 1).is_none());
    }

    #[test]
    fn bracket_object_resolves() {
        let mut s = mk("a[b]c");
        s.cursor_pos = Position::new(2, 0);
        let (lo, hi, _) = s.text_object_range(TextObject::Bracket, true, 1).unwrap();
        assert_eq!(&s.text_at(lo, hi), "[b]");
    }

    #[test]
    fn brace_object_resolves() {
        let mut s = mk("x { y } z");
        s.cursor_pos = Position::new(4, 0);
        let (lo, hi, _) = s.text_object_range(TextObject::Brace, false, 1).unwrap();
        assert_eq!(&s.text_at(lo, hi), " y ");
    }

    #[test]
    fn pair_works_across_lines() {
        let mut s = mk("foo(\n  bar\n)baz");
        s.cursor_pos = Position::new(2, 1);
        let (lo, hi, _) = s.text_object_range(TextObject::Paren, false, 1).unwrap();
        assert_eq!(&s.text_at(lo, hi), "\n  bar\n");
    }

    // ---- quote ----------------------------------------------------------

    #[test]
    fn i_dquote_inner_text() {
        let mut s = mk(r#"x = "hello world" y"#);
        s.cursor_pos = Position::new(8, 0);
        let (lo, hi, _) = s
            .text_object_range(TextObject::DoubleQuote, false, 1)
            .unwrap();
        assert_eq!(&s.text_at(lo, hi), "hello world");
    }

    #[test]
    fn a_dquote_includes_quotes() {
        let mut s = mk(r#"x = "hello world" y"#);
        s.cursor_pos = Position::new(8, 0);
        let (lo, hi, _) = s
            .text_object_range(TextObject::DoubleQuote, true, 1)
            .unwrap();
        // Vim's `a"` extends with trailing whitespace (the ` ` before `y`).
        assert_eq!(&s.text_at(lo, hi), "\"hello world\" ");
    }

    #[test]
    fn quote_skips_escaped_quotes() {
        let mut s = mk(r#""he said \"hi\" then""#);
        s.cursor_pos = Position::new(15, 0);
        let (lo, hi, _) = s
            .text_object_range(TextObject::DoubleQuote, false, 1)
            .unwrap();
        assert_eq!(&s.text_at(lo, hi), r#"he said \"hi\" then"#);
    }

    #[test]
    fn quote_picks_next_pair_when_cursor_in_gap() {
        let mut s = mk(r#"a "b" c "d" e"#);
        s.cursor_pos = Position::new(6, 0);
        let (lo, hi, _) = s
            .text_object_range(TextObject::DoubleQuote, false, 1)
            .unwrap();
        assert_eq!(&s.text_at(lo, hi), "d");
    }

    #[test]
    fn single_quote_object() {
        let mut s = mk("'foo' bar");
        s.cursor_pos = Position::new(2, 0);
        let (lo, hi, _) = s
            .text_object_range(TextObject::SingleQuote, false, 1)
            .unwrap();
        assert_eq!(&s.text_at(lo, hi), "foo");
    }

    #[test]
    fn quote_returns_none_when_only_one_quote() {
        let mut s = mk(r#"only "one"#);
        s.cursor_pos = Position::new(7, 0);
        assert!(
            s.text_object_range(TextObject::DoubleQuote, false, 1)
                .is_none()
        );
    }

    // ---- FromStr --------------------------------------------------------

    #[test]
    fn from_str_accepts_friendly_aliases() {
        assert_eq!(TextObject::from_str("word"), Ok(TextObject::Word));
        assert_eq!(TextObject::from_str("("), Ok(TextObject::Paren));
        assert_eq!(TextObject::from_str("b"), Ok(TextObject::Paren));
        assert_eq!(TextObject::from_str("B"), Ok(TextObject::Brace));
        assert_eq!(TextObject::from_str("\""), Ok(TextObject::DoubleQuote));
        assert!(TextObject::from_str("nope").is_err());
    }
}
