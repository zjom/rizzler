//! Vim-style named registers.
//!
//! [`Registers`] is the single owner of every register the editor exposes —
//! the unnamed (`"`), yank (`0`), numbered (`1`-`9`), small-delete (`-`),
//! named (`a`-`z`, with `A`-`Z` appending), last-search (`/`),
//! last-command (`:`), last-insert (`.`), and the black-hole (`_`) sinks.
//!
//! The editor never touches the internal map directly — instead it calls
//! the routing helpers ([`record_yank`](Registers::record_yank),
//! [`record_delete`](Registers::record_delete), …) which mirror vim's
//! distribution rules so callers only have to say *what happened*, not
//! *where to file it*. Reads go through [`read`](Registers::read) which
//! resolves `"`/`@` as aliases for the unnamed register.

use std::collections::HashMap;
use std::rc::Rc;

/// Whether a register's contents represent a character run, whole lines, or
/// a rectangular block. Paste behavior depends on the kind: linewise pastes
/// open a new line; charwise inserts inline; blockwise inserts column-wise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RegisterKind {
    Char,
    Line,
    Block,
}

/// A register's stored content — the text plus the kind that drives paste
/// behavior.
#[derive(Debug, Clone)]
pub struct RegisterEntry {
    pub text: Rc<str>,
    pub kind: RegisterKind,
}

impl RegisterEntry {
    pub fn new(text: impl Into<Rc<str>>, kind: RegisterKind) -> Self {
        Self {
            text: text.into(),
            kind,
        }
    }

    pub fn charwise(text: impl Into<Rc<str>>) -> Self {
        Self::new(text, RegisterKind::Char)
    }

    pub fn linewise(text: impl Into<Rc<str>>) -> Self {
        Self::new(text, RegisterKind::Line)
    }
}

/// The canonical name used to read/write the unnamed (default) register.
/// `"`/`@` are also accepted as aliases — see [`normalize_name`].
pub const UNNAMED: char = '"';
/// The yank-only register. Vim writes here on every yank (in addition to `"`)
/// but never on a delete.
pub const YANK: char = '0';
/// The small-delete register — receives charwise deletes that don't cross a
/// newline.
pub const SMALL_DELETE: char = '-';
/// Last successful search pattern.
pub const LAST_SEARCH: char = '/';
/// Last `:`-command typed.
pub const LAST_COMMAND: char = ':';
/// Last inserted text (vim's `.`).
pub const LAST_INSERT: char = '.';
/// Black-hole register — writes are dropped.
pub const BLACK_HOLE: char = '_';

/// All registers, addressed by character. Routing helpers
/// (`record_yank`, `record_delete`, …) implement vim's distribution rules so
/// callers only describe what just happened.
#[derive(Debug, Clone, Default)]
pub struct Registers {
    by_name: HashMap<char, RegisterEntry>,
}

impl Registers {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read register `name`. `"` and `@` alias the unnamed register, and an
    /// upper-case named register reads from its lower-case slot. The
    /// black-hole register `_` always returns `None`.
    pub fn read(&self, name: char) -> Option<&RegisterEntry> {
        let n = normalize_name(name)?;
        if n == BLACK_HOLE {
            return None;
        }
        self.by_name.get(&n)
    }

    /// Replace register `name`. `A`-`Z` appends to the lower-case slot
    /// (keeping the existing kind unless the slot is empty). Writes to the
    /// black-hole register are silently dropped, matching vim.
    pub fn write(&mut self, name: char, entry: RegisterEntry) {
        let Some(n) = normalize_name(name) else {
            return;
        };
        if n == BLACK_HOLE {
            return;
        }
        if name.is_ascii_uppercase() {
            self.append(n, entry);
        } else {
            self.by_name.insert(n, entry);
        }
    }

    fn append(&mut self, slot: char, entry: RegisterEntry) {
        match self.by_name.get(&slot) {
            Some(existing) => {
                let mut joined = String::with_capacity(existing.text.len() + entry.text.len());
                joined.push_str(&existing.text);
                if existing.kind == RegisterKind::Line && !joined.ends_with('\n') {
                    joined.push('\n');
                }
                joined.push_str(&entry.text);
                let kind = existing.kind;
                self.by_name.insert(
                    slot,
                    RegisterEntry {
                        text: Rc::from(joined),
                        kind,
                    },
                );
            }
            None => {
                self.by_name.insert(slot, entry);
            }
        }
    }

    /// Convenience read of the unnamed register — the default paste source.
    pub fn unnamed(&self) -> Option<&RegisterEntry> {
        self.by_name.get(&UNNAMED)
    }

    /// Route a yank into the appropriate set of registers. Vim fills:
    ///
    /// * the unnamed register `"` always,
    /// * the yank register `0` always (independent of `name`),
    /// * the explicit target `name` if given (`A`-`Z` appends).
    ///
    /// `name == Some('_')` discards the text entirely (black-hole).
    pub fn record_yank(
        &mut self,
        text: impl Into<Rc<str>>,
        kind: RegisterKind,
        name: Option<char>,
    ) {
        let text: Rc<str> = text.into();
        if matches!(name, Some(BLACK_HOLE)) {
            return;
        }
        let entry = RegisterEntry::new(text.clone(), kind);
        self.by_name.insert(UNNAMED, entry.clone());
        self.by_name.insert(YANK, entry.clone());
        if let Some(n) = name {
            self.write(n, entry);
        }
    }

    /// Route a delete into the appropriate set of registers. Vim fills:
    ///
    /// * the unnamed register `"` always,
    /// * the small-delete register `-` for charwise deletes that don't span
    ///   a line break,
    /// * the numbered registers `1`-`9` for line/block deletes and any
    ///   delete that crosses a line — `1` rolls down to `2`, `2` to `3`, etc.,
    /// * the explicit target `name` if given (`A`-`Z` appends; `_` discards).
    pub fn record_delete(
        &mut self,
        text: impl Into<Rc<str>>,
        kind: RegisterKind,
        name: Option<char>,
    ) {
        let text: Rc<str> = text.into();
        if matches!(name, Some(BLACK_HOLE)) {
            return;
        }
        let entry = RegisterEntry::new(text.clone(), kind);
        self.by_name.insert(UNNAMED, entry.clone());
        if let Some(n) = name {
            self.write(n, entry.clone());
        }
        if is_small_delete(&entry) {
            self.by_name.insert(SMALL_DELETE, entry);
        } else {
            self.rotate_numbered(entry);
        }
    }

    fn rotate_numbered(&mut self, entry: RegisterEntry) {
        for slot in ('1'..='8').rev() {
            if let Some(v) = self.by_name.remove(&slot) {
                let next = (slot as u8 + 1) as char;
                self.by_name.insert(next, v);
            }
        }
        self.by_name.insert('1', entry);
    }

    /// Record the last inserted text into `.`. No-op for empty strings.
    pub fn record_insert(&mut self, text: impl Into<Rc<str>>) {
        let text: Rc<str> = text.into();
        if text.is_empty() {
            return;
        }
        self.by_name
            .insert(LAST_INSERT, RegisterEntry::charwise(text));
    }

    /// Record the last successful search pattern into `/`.
    pub fn record_search(&mut self, pattern: impl Into<Rc<str>>) {
        self.by_name
            .insert(LAST_SEARCH, RegisterEntry::charwise(pattern));
    }

    /// Record the most recent `:`-command into `:`.
    pub fn record_command(&mut self, cmd: impl Into<Rc<str>>) {
        self.by_name
            .insert(LAST_COMMAND, RegisterEntry::charwise(cmd));
    }

    /// Drop every register. Mostly useful in tests.
    pub fn clear(&mut self) {
        self.by_name.clear();
    }

    /// Iterate every populated register, ordered by name. Used by inspector
    /// UIs (`:reg`).
    pub fn iter(&self) -> impl Iterator<Item = (char, &RegisterEntry)> {
        let mut entries: Vec<_> = self.by_name.iter().map(|(k, v)| (*k, v)).collect();
        entries.sort_by_key(|(k, _)| *k);
        entries.into_iter()
    }
}

/// Map an externally-provided register name to the canonical slot key.
///
/// * `A`-`Z` → lower-case (with `write`'s append semantics layered on top).
/// * `"` and `@` → the unnamed slot.
/// * any other ASCII letter/digit or punctuation register name passes through.
/// * everything else (control chars, multi-byte) → `None`.
pub fn normalize_name(name: char) -> Option<char> {
    match name {
        '"' | '@' => Some(UNNAMED),
        c if c.is_ascii_uppercase() => Some(c.to_ascii_lowercase()),
        c if c.is_ascii_lowercase() || c.is_ascii_digit() => Some(c),
        '-' | '/' | ':' | '.' | '_' | '%' | '#' | '*' | '+' | '=' => Some(name),
        _ => None,
    }
}

fn is_small_delete(entry: &RegisterEntry) -> bool {
    entry.kind == RegisterKind::Char && !entry.text.contains('\n')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(r: &Registers, name: char) -> Option<&str> {
        r.read(name).map(|e| e.text.as_ref())
    }

    #[test]
    fn yank_fills_unnamed_and_zero() {
        let mut r = Registers::new();
        r.record_yank("hello", RegisterKind::Char, None);
        assert_eq!(text(&r, '"'), Some("hello"));
        assert_eq!(text(&r, '0'), Some("hello"));
        assert!(r.read('1').is_none());
    }

    #[test]
    fn yank_to_named_also_fills_zero_and_unnamed() {
        let mut r = Registers::new();
        r.record_yank("hi", RegisterKind::Char, Some('a'));
        assert_eq!(text(&r, 'a'), Some("hi"));
        assert_eq!(text(&r, '"'), Some("hi"));
        assert_eq!(text(&r, '0'), Some("hi"));
    }

    #[test]
    fn uppercase_named_appends() {
        let mut r = Registers::new();
        r.record_yank("foo", RegisterKind::Char, Some('a'));
        r.record_yank("bar", RegisterKind::Char, Some('A'));
        assert_eq!(text(&r, 'a'), Some("foobar"));
    }

    #[test]
    fn uppercase_linewise_append_joins_with_newline() {
        let mut r = Registers::new();
        r.record_yank("foo", RegisterKind::Line, Some('a'));
        r.record_yank("bar", RegisterKind::Line, Some('A'));
        assert_eq!(text(&r, 'a'), Some("foo\nbar"));
    }

    #[test]
    fn small_delete_fills_dash_not_numbered() {
        let mut r = Registers::new();
        r.record_delete("hi", RegisterKind::Char, None);
        assert_eq!(text(&r, '-'), Some("hi"));
        assert_eq!(text(&r, '"'), Some("hi"));
        assert!(r.read('1').is_none());
        // The yank register is untouched by deletes.
        assert!(r.read('0').is_none());
    }

    #[test]
    fn linewise_delete_rotates_numbered() {
        let mut r = Registers::new();
        r.record_delete("first\n", RegisterKind::Line, None);
        assert_eq!(text(&r, '1'), Some("first\n"));
        r.record_delete("second\n", RegisterKind::Line, None);
        assert_eq!(text(&r, '1'), Some("second\n"));
        assert_eq!(text(&r, '2'), Some("first\n"));
    }

    #[test]
    fn black_hole_drops_writes() {
        let mut r = Registers::new();
        r.record_delete("hi", RegisterKind::Char, Some('_'));
        assert!(r.read('_').is_none());
        assert!(r.read('"').is_none());
    }

    #[test]
    fn quote_and_at_alias_unnamed() {
        let mut r = Registers::new();
        r.write('"', RegisterEntry::charwise("payload"));
        assert_eq!(text(&r, '"'), Some("payload"));
        assert_eq!(text(&r, '@'), Some("payload"));
    }

    #[test]
    fn record_insert_drops_empty() {
        let mut r = Registers::new();
        r.record_insert("");
        assert!(r.read('.').is_none());
        r.record_insert("typed");
        assert_eq!(text(&r, '.'), Some("typed"));
    }

    #[test]
    fn iter_sorts_by_name() {
        let mut r = Registers::new();
        r.record_yank("hi", RegisterKind::Char, Some('a'));
        r.record_delete("bye\n", RegisterKind::Line, None);
        let names: Vec<char> = r.iter().map(|(c, _)| c).collect();
        assert_eq!(names, vec!['"', '0', '1', 'a']);
    }
}
