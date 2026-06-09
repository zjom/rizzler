//! Rope-backed text buffer.
//!
//! `Buffer` is the central editing primitive of the editor: it owns a rope,
//! a cursor + viewport position, a mode (with optional pushed mode layers),
//! a [`crate::props::PropStore`] of text properties + overlays, a
//! [`crate::wrap::WrapSettings`] block + cached [`crate::wrap::WrapMap`], and
//! a [`rizz_changetree::ChangeTree`] for undo/redo.
//!
//! The implementation is split across submodules by concern so each file
//! stays navigable:
//! - `cursor` — cursor movement, scrolling, clamping
//! - `edits` — insert/delete + undo/redo
//! - `marks` — selection anchor + keymap mode layers
//!
//! The fields stay `pub(crate)` because submodules + `crate::io` touch them
//! directly; everything else goes through the accessor methods on `Buffer`.

mod cursor;
mod edits;
mod marks;
mod text_object;
mod yank;

use std::{path::Path, rc::Rc, str::FromStr};

use rizz_changetree::ChangeTree;
use rizz_ts::{Highlighter, Point};
use ropey::{Rope, RopeSlice, iter::Lines};

use rizz_core::{EditingMode, Position};

use crate::{
    props::PropStore,
    wrap::{WrapMap, WrapMode, WrapSettings},
};

pub use cursor::MoveKind;
pub use edits::{ReplaceBatch, Speculation};
pub use text_object::TextObject;

slotmap::new_key_type! {
    /// Stable handle to a `Buffer` held by the editor's buffer registry.
    ///
    /// Issued by `slotmap::SlotMap` so the value survives insertions/removals
    /// of unrelated buffers — no manual reindex on delete. Window leaves,
    /// popups, and the `Widget::BufferView` widget all reference buffers by
    /// `BufferId` so they can't go stale.
    pub struct BufferId;
}

#[derive(Debug, Clone, Default)]
pub struct Buffer {
    pub(crate) buf: Rope,
    pub(crate) cursor_pos: Position<u16>,
    pub(crate) file_pos: Position<usize>,
    pub(crate) fs_path: Option<Rc<Path>>,
    /// Visible viewport size in cells. When `viewport.row > 0`, cursor
    /// movement scrolls `file_pos` to keep the cursor in view. Default zero
    /// means "no viewport" — scrolling is a no-op (useful in tests).
    pub viewport: Position<u16>,
    pub(crate) mode: EditingMode,
    /// Anchor (absolute file position) of the current visual selection.
    /// `Some` iff `mode` is one of the visual modes — managed by `set_mode`.
    pub(crate) selection_anchor: Option<Position<usize>>,
    /// Sticky column for vertical motion. Initialized on the first vertical
    /// `Relative` step and preserved across subsequent ones so traversing a
    /// short line doesn't truncate the cursor's column on later longer ones.
    /// Cleared by anything that breaks the run — non-vertical cursor moves,
    /// edits, mode changes — see [`Buffer::close_insert_batch`].
    pub(crate) goal_col: Option<usize>,
    /// Text properties and overlays. Built up by lisp via
    /// `put-text-property` / `overlay-create`; consumed by the precompute
    /// pass to emit decorator ranges.
    pub(crate) props: PropStore,
    /// Soft-wrap configuration. When `wrap.mode` is non-`None`, the
    /// precompute pass builds a `WrapMap` for this buffer and the renderer
    /// emits one visual row per `WrapMap` entry.
    pub(crate) wrap: WrapSettings,
    /// Cached visual-line layout from the most recent render. Movement code
    /// reads it to step in visual rows; `None` means "no recent render" or
    /// "wrap is off" — fall back to file-row movement.
    pub(crate) wrap_cache: Option<WrapMap>,
    pub(crate) changetree: ChangeTree,
    /// When `Some(end_char)`, the current changetree leaf is an open insert
    /// batch: the next `insert_char` whose target char index equals
    /// `end_char` extends that leaf in place instead of pushing a new node.
    /// Cleared by anything that breaks the run — a non-insert edit, a
    /// cursor move, a mode change, undo/redo.
    pub(crate) insert_batch_end: Option<usize>,
    /// Active speculative insertion staged by the keymap during a chord
    /// prefix. `insert_speculative_char` writes to the rope but defers
    /// tracking until `commit_speculation` (chord aborts → text stays,
    /// recorded as one delta) or `rollback_speculation` (chord completes →
    /// chars are unwound, leaving no trace in undo history).
    pub(crate) speculation: Option<Speculation>,
    /// Active Replace-mode session. Set when the buffer enters
    /// [`EditingMode::Replace`]; cleared on exit or whenever a non-overwrite
    /// edit / cursor move forces a flush. Records, for each typed char,
    /// whether it overwrote an existing char (`Some(orig)` — `<bs>` restores
    /// orig) or was inserted past EOL (`None` — `<bs>` deletes it). The
    /// whole session lands as a single tracked delta on commit.
    pub(crate) replace_batch: Option<ReplaceBatch>,
    /// Optional syntax-highlighter. Installed by `State` after consulting
    /// the `TsRegistry` for a grammar matching the file extension. Edits
    /// forward incremental rope splices via [`Buffer::record_highlight_edit`];
    /// the precompute pass refreshes the source snapshot and reparses on demand.
    pub(crate) highlight: Option<Highlighter>,
}

impl Buffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fs_path(&self) -> Option<Rc<Path>> {
        self.fs_path.clone()
    }

    pub fn props(&self) -> &PropStore {
        &self.props
    }

    pub fn props_mut(&mut self) -> &mut PropStore {
        &mut self.props
    }

    pub fn mode(&self) -> EditingMode {
        self.mode
    }

    pub fn wrap_mode(&self) -> WrapMode {
        self.wrap.mode
    }

    pub fn set_wrap_mode(&mut self, m: WrapMode) {
        self.wrap.mode = m;
    }

    pub fn wrap_column(&self) -> Option<u16> {
        self.wrap.column
    }

    pub fn set_wrap_column(&mut self, col: Option<u16>) {
        self.wrap.column = col;
    }

    pub fn breakindent(&self) -> bool {
        self.wrap.breakindent
    }

    pub fn set_breakindent(&mut self, b: bool) {
        self.wrap.breakindent = b;
    }

    /// Drop the cached visual-line layout. Called by every edit so the next
    /// render builds a fresh map.
    pub(crate) fn invalidate_wrap_cache(&mut self) {
        self.wrap_cache = None;
    }

    /// Drop the tree-sitter tree wholesale so the next render full-reparses.
    /// Reserved for paths that swap the entire rope (`clear`, `clear_with`,
    /// file reloads) — incremental reuse only makes sense when every
    /// intervening edit has been described to the highlighter via
    /// [`Buffer::record_highlight_edit`].
    pub(crate) fn reset_highlight(&mut self) {
        if let Some(h) = &mut self.highlight {
            h.invalidate();
        }
    }

    /// Forward a single rope splice to the highlighter so tree-sitter can
    /// reuse subtrees outside the edit region. `at_char` is the rope char
    /// index where the splice starts; `removed` / `inserted` are the strings
    /// pulled out and pushed in. Safe to call before or after the rope
    /// mutation — `at_char`'s byte/point coordinates depend only on the
    /// unchanged prefix.
    pub(crate) fn record_highlight_edit(&mut self, at_char: usize, removed: &str, inserted: &str) {
        if self.highlight.is_none() {
            return;
        }
        let start_byte = self.buf.char_to_byte(at_char);
        let row = self.buf.char_to_line(at_char);
        let line_start_byte = self.buf.line_to_byte(row);
        let start_position = Point {
            row,
            column: start_byte - line_start_byte,
        };
        let old_end_position = advance_point(start_position, removed);
        let new_end_position = advance_point(start_position, inserted);
        let old_end_byte = start_byte + removed.len();
        let new_end_byte = start_byte + inserted.len();
        if let Some(h) = self.highlight.as_mut() {
            h.record_edit(
                start_byte,
                old_end_byte,
                new_end_byte,
                start_position,
                old_end_position,
                new_end_position,
            );
        }
    }

    /// Install (or remove) a fully-constructed highlighter. The editor's
    /// `State::install_dynamic_highlighter` calls this after consulting the
    /// `TsRegistry` populated by the `(grammar-register ...)` lisp builtin.
    pub fn set_highlighter(&mut self, h: Option<Highlighter>) {
        self.highlight = h;
    }

    pub fn highlight_mut(&mut self) -> Option<&mut Highlighter> {
        self.highlight.as_mut()
    }

    pub fn highlight(&self) -> Option<&Highlighter> {
        self.highlight.as_ref()
    }

    /// If a highlighter is attached and dirty, snapshot the rope into it and
    /// reparse. Cheap when clean: the dirty flag short-circuits before any
    /// allocation. Called from `State::precompute_frame` before the precompute
    /// pass walks buffers immutably.
    pub fn refresh_highlight(&mut self) {
        if !self.highlight.as_ref().is_some_and(|h| h.is_dirty()) {
            return;
        }
        let src = self.buf.to_string();
        if let Some(h) = self.highlight.as_mut() {
            h.set_source(src);
            h.ensure_parsed();
        }
    }

    /// End every in-flight coalescing batch — the insert run, the sticky
    /// goal column, and any Replace-mode session (which flushes to the
    /// changetree). Called by anything that breaks one of those: a
    /// non-insert edit, a cursor move, a mode change, undo/redo. The
    /// `overwrite_char` / `replace_backspace` pair deliberately skips this
    /// path so Replace-mode keystrokes coalesce into one delta. Callers
    /// that need to preserve the goal column across this call (notably
    /// vertical `move_cursor` steps) capture and restore it themselves.
    pub(crate) fn close_insert_batch(&mut self) {
        self.commit_replace_batch();
        self.insert_batch_end = None;
        self.goal_col = None;
    }

    /// Most recent visual-line layout (from the last render's precompute
    /// pass). Movement and scroll code reads this when wrap is on.
    pub fn wrap_cache(&self) -> Option<&WrapMap> {
        self.wrap_cache.as_ref()
    }

    pub fn set_wrap_cache(&mut self, map: Option<WrapMap>) {
        self.wrap_cache = map;
    }

    /// Cursor's absolute file position (file_pos + cursor_pos).
    pub fn abs_pos(&self) -> Position<usize> {
        Position::new(
            self.file_pos.col + self.cursor_pos.col as usize,
            self.file_pos.row + self.cursor_pos.row as usize,
        )
    }

    /// Cursor's absolute file row — `file_pos.row + cursor_pos.row`.
    pub fn abs_row(&self) -> usize {
        self.file_pos.row + self.cursor_pos.row as usize
    }

    /// Cursor's absolute file column — `file_pos.col + cursor_pos.col`.
    pub fn abs_col(&self) -> usize {
        self.file_pos.col + self.cursor_pos.col as usize
    }

    /// Reset rope content and cursor — used when the minibuffer finishes
    /// processing a command and needs to be empty again.
    pub fn clear(&mut self) {
        self.buf = Rope::new();
        self.cursor_pos = Position::default();
        self.file_pos = Position::default();
        self.invalidate_wrap_cache();
        self.reset_highlight();
    }

    pub fn clear_with(&mut self, text: &str) {
        self.buf = Rope::from_str(text);
        self.invalidate_wrap_cache();
        self.reset_highlight();
        self.clamp_cursor();
    }

    /// Owned snapshot of the rope text — used by command parsing.
    pub fn text(&self) -> String {
        self.buf.to_string()
    }

    pub fn cursor_pos(&self) -> Position<u16> {
        self.cursor_pos
    }

    pub fn file_pos(&self) -> Position<usize> {
        self.file_pos
    }

    pub fn len_lines(&self) -> usize {
        self.buf.len_lines()
    }

    pub fn lines_at(&self, idx: usize) -> Lines<'_> {
        self.buf.lines_at(idx)
    }

    /// Read-only handle to the underlying rope. Used by callers that need
    /// byte/char conversions (e.g. the tree-sitter highlight pass).
    pub fn rope(&self) -> &Rope {
        &self.buf
    }

    pub(crate) fn cur_line(&self) -> RopeSlice<'_> {
        self.buf.line(self.cur_lnum())
    }

    pub(crate) fn cur_lnum(&self) -> usize {
        self.abs_row()
    }

    pub(crate) fn cur_line_start(&self) -> usize {
        self.buf.line_to_char(self.cur_lnum())
    }
}

/// Advance a tree-sitter [`Point`] forward across `s`. Tree-sitter measures
/// column in bytes from the line start, so newlines reset the column and
/// non-newline chars advance by their UTF-8 length.
fn advance_point(start: Point, s: &str) -> Point {
    let mut row = start.row;
    let mut column = start.column;
    for ch in s.chars() {
        if ch == '\n' {
            row += 1;
            column = 0;
        } else {
            column += ch.len_utf8();
        }
    }
    Point { row, column }
}

impl FromStr for Buffer {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self {
            buf: Rope::from_str(s),
            ..Self::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(text: &str) -> Buffer {
        Buffer::from_str(text).expect("never fails")
    }

    fn cur_row(s: &Buffer) -> usize {
        s.cursor_pos.row as usize + s.file_pos.row
    }

    fn cur_col(s: &Buffer) -> usize {
        s.cursor_pos.col as usize + s.file_pos.col
    }

    // ---- insert_char --------------------------------------------------

    #[test]
    fn insert_into_empty_buffer() {
        let mut s = mk("");
        s.insert_char('a');
        assert_eq!(s.buf.to_string(), "a");
        assert_eq!(s.cursor_pos.row, 0);
        assert_eq!(s.cursor_pos.col, 1);
    }

    #[test]
    fn insert_on_second_line_uses_correct_offset() {
        let mut s = mk("ab\ncd");
        s.cursor_pos = Position::<u16>::new(1, 1);
        s.insert_char('X');
        assert_eq!(s.buf.to_string(), "ab\ncXd");
        assert_eq!(s.cursor_pos.row, 1);
        assert_eq!(s.cursor_pos.col, 2);
    }

    #[test]
    fn insert_newline_splits_line() {
        let mut s = mk("abcd");
        s.cursor_pos = Position::<u16>::new(2, 0);
        s.insert_char('\n');
        assert_eq!(s.buf.to_string(), "ab\ncd");
        assert_eq!(s.cursor_pos.row, 1);
        assert_eq!(s.cursor_pos.col, 0);
    }

    #[test]
    fn insert_at_end_of_buffer() {
        let mut s = mk("ab");
        s.cursor_pos = Position::<u16>::new(2, 0);
        s.insert_char('c');
        assert_eq!(s.buf.to_string(), "abc");
        assert_eq!(s.cursor_pos.col, 3);
    }

    // ---- delete_char --------------------------------------------------

    #[test]
    fn delete_at_file_start_is_noop() {
        let mut s = mk("hello");
        s.cursor_pos = Position::<u16>::new(0, 0);
        s.delete_char();
        assert_eq!(s.buf.to_string(), "hello");
    }

    #[test]
    fn delete_char_in_middle() {
        let mut s = mk("hello");
        s.cursor_pos = Position::<u16>::new(3, 0);
        s.delete_char();
        assert_eq!(s.buf.to_string(), "helo");
        assert_eq!(s.cursor_pos.col, 2);
    }

    #[test]
    fn delete_only_character() {
        let mut s = mk("a");
        s.cursor_pos = Position::<u16>::new(1, 0);
        s.delete_char();
        assert_eq!(s.buf.to_string(), "");
    }

    #[test]
    fn delete_newline_at_line_start() {
        let mut s = mk("ab\ncd\nef");
        s.cursor_pos = Position::<u16>::new(0, 2);
        s.delete_char();
        assert_eq!(s.buf.to_string(), "ab\ncdef");
        assert_eq!(s.cursor_pos.row, 1);
        assert_eq!(s.cursor_pos.col, 2);
    }

    // ---- move_cursor: LineStart / LineEnd -----------------------------

    #[test]
    fn line_start_moves_to_col_zero() {
        let mut s = mk("hello\nworld");
        s.cursor_pos = Position::<u16>::new(3, 1);
        s.move_cursor(MoveKind::LineStart);
        assert_eq!(s.cursor_pos.col, 0);
    }

    #[test]
    fn line_end_does_not_land_on_newline() {
        let mut s = mk("abc\ndef");
        s.mode = EditingMode::Insert;
        s.move_cursor(MoveKind::LineEnd);
        assert_eq!(s.cursor_pos.col, 3);
    }
    #[test]
    fn line_end_lands_on_last_char_in_normal_mode() {
        let mut s = mk("abc\ndef");
        s.mode = EditingMode::Normal;
        s.move_cursor(MoveKind::LineEnd);
        assert_eq!(s.cursor_pos.col, 2);
    }

    #[test]
    fn line_end_on_last_line_without_newline() {
        let mut s = mk("abc\ndef");
        s.cursor_pos = Position::<u16>::new(0, 1);
        s.move_cursor(MoveKind::LineEnd);
        assert_eq!(s.cursor_pos.col, 2);
    }

    #[test]
    fn file_start_resets_row_and_col() {
        let mut s = mk("hello\nworld");
        s.cursor_pos = Position::<u16>::new(3, 1);
        s.move_cursor(MoveKind::FileStart);
        assert_eq!(cur_row(&s), 0);
        assert_eq!(cur_col(&s), 0);
    }

    #[test]
    fn file_end_moves_to_last_line() {
        let mut s = mk("a\nb\nc");
        s.move_cursor(MoveKind::FileEnd);
        assert_eq!(cur_row(&s), 2);
    }

    #[test]
    fn line_num_moves_to_specified_line() {
        let mut s = mk("a\nb\nc\nd\ne");
        s.move_cursor(MoveKind::LineNum(2));
        assert_eq!(cur_row(&s), 2);
    }

    #[test]
    fn line_num_clamps_to_last_line() {
        let mut s = mk("a\nb\nc");
        s.move_cursor(MoveKind::LineNum(100));
        assert_eq!(cur_row(&s), 2);
    }

    // ---- word motions -------------------------------------------------

    #[test]
    fn word_end_lands_on_last_char_of_word() {
        let mut s = mk("hello world");
        s.move_cursor(MoveKind::WordEnd);
        assert_eq!(s.cursor_pos.col, 4);
    }

    #[test]
    fn word_end_jumps_to_next_word_when_already_at_end() {
        let mut s = mk("hello world");
        s.cursor_pos = Position::<u16>::new(4, 0);
        s.move_cursor(MoveKind::WordEnd);
        assert_eq!(s.cursor_pos.col, 10);
    }

    #[test]
    fn word_start_goes_to_previous_word_start() {
        let mut s = mk("hello world foo");
        s.cursor_pos = Position::<u16>::new(8, 0);
        s.move_cursor(MoveKind::WordStart);
        assert_eq!(s.cursor_pos.col, 6);
    }

    #[test]
    fn word_forward_jumps_to_next_word_start() {
        let mut s = mk("hello world foo");
        s.move_cursor(MoveKind::WordForward);
        assert_eq!(s.cursor_pos.col, 6);
    }

    #[test]
    fn word_forward_skips_over_multiple_spaces() {
        let mut s = mk("a    b");
        s.move_cursor(MoveKind::WordForward);
        assert_eq!(s.cursor_pos.col, 5);
    }

    #[test]
    fn word_back_end_from_inside_word_lands_on_prev_word_end() {
        let mut s = mk("hello world foo");
        s.cursor_pos = Position::<u16>::new(8, 0);
        s.move_cursor(MoveKind::WordBackEnd);
        assert_eq!(s.cursor_pos.col, 4);
    }

    #[test]
    fn big_word_forward_treats_punctuation_as_word_char() {
        let mut s = mk("foo.bar baz");
        s.move_cursor(MoveKind::BigWordForward);
        assert_eq!(s.cursor_pos.col, 8);
    }

    #[test]
    fn word_forward_splits_on_punctuation() {
        let mut s = mk("foo.bar");
        s.move_cursor(MoveKind::WordForward);
        assert_eq!(s.cursor_pos.col, 3);
        s.move_cursor(MoveKind::WordForward);
        assert_eq!(s.cursor_pos.col, 4);
    }

    #[test]
    fn word_forward_crosses_newline_to_next_line() {
        let mut s = mk("foo\nbar");
        s.move_cursor(MoveKind::WordForward);
        assert_eq!(s.cursor_pos.row, 1);
        assert_eq!(s.cursor_pos.col, 0);
    }

    // ---- match-bracket (vim `%`) ------------------------------------

    #[test]
    fn match_bracket_jumps_forward_to_close_paren() {
        let mut s = mk("(abc)");
        s.move_cursor(MoveKind::MatchBracket);
        assert_eq!(s.cursor_pos.col, 4);
    }

    #[test]
    fn match_bracket_jumps_back_to_open_paren_from_close() {
        let mut s = mk("(abc)");
        s.cursor_pos = Position::<u16>::new(4, 0);
        s.move_cursor(MoveKind::MatchBracket);
        assert_eq!(s.cursor_pos.col, 0);
    }

    #[test]
    fn match_bracket_scans_line_when_cursor_off_bracket() {
        let mut s = mk("foo (bar) baz");
        s.move_cursor(MoveKind::MatchBracket);
        assert_eq!(s.cursor_pos.col, 8);
    }

    #[test]
    fn match_bracket_handles_nesting() {
        let mut s = mk("(a(b)c)");
        s.move_cursor(MoveKind::MatchBracket);
        assert_eq!(s.cursor_pos.col, 6);
    }

    #[test]
    fn match_bracket_matches_braces_and_brackets() {
        let mut s = mk("{[x]}");
        s.move_cursor(MoveKind::MatchBracket);
        assert_eq!(s.cursor_pos.col, 4);
        s.cursor_pos = Position::<u16>::new(1, 0);
        s.move_cursor(MoveKind::MatchBracket);
        assert_eq!(s.cursor_pos.col, 3);
    }

    #[test]
    fn match_bracket_crosses_lines() {
        let mut s = mk("(\n  a\n)");
        s.move_cursor(MoveKind::MatchBracket);
        assert_eq!(s.cursor_pos.row, 2);
        assert_eq!(s.cursor_pos.col, 0);
    }

    #[test]
    fn match_bracket_no_bracket_on_line_is_noop() {
        let mut s = mk("hello world");
        s.cursor_pos = Position::<u16>::new(3, 0);
        s.move_cursor(MoveKind::MatchBracket);
        assert_eq!(s.cursor_pos.col, 3);
    }

    #[test]
    fn match_bracket_unmatched_open_stays_put() {
        let mut s = mk("(abc");
        s.move_cursor(MoveKind::MatchBracket);
        assert_eq!(s.cursor_pos.col, 0);
    }

    #[test]
    fn line_first_non_blank_skips_leading_whitespace() {
        let mut s = mk("    hello");
        s.cursor_pos = Position::<u16>::new(7, 0);
        s.move_cursor(MoveKind::LineFirstNonBlank);
        assert_eq!(s.cursor_pos.col, 4);
    }

    #[test]
    fn line_first_non_blank_on_blank_line_stays_at_zero() {
        let mut s = mk("   \nabc");
        s.cursor_pos = Position::<u16>::new(2, 0);
        s.move_cursor(MoveKind::LineFirstNonBlank);
        assert_eq!(s.cursor_pos.col, 0);
    }

    // ---- move_cursor_n -----------------------------------------------

    #[test]
    fn move_cursor_n_scales_relative_delta() {
        let mut s = mk("a\nb\nc\nd\ne");
        s.move_cursor_n(MoveKind::Relative(Position::new(0, 1)), 3);
        assert_eq!(cur_row(&s), 3);
    }

    #[test]
    fn move_cursor_n_loops_for_non_relative_kinds() {
        let mut s = mk("aaa bbb ccc ddd");
        s.move_cursor_n(MoveKind::WordForward, 2);
        assert_eq!(s.cursor_pos.col, 8);
    }

    #[test]
    fn move_cursor_n_count_zero_runs_once() {
        let mut s = mk("a\nb\nc");
        s.move_cursor_n(MoveKind::Relative(Position::new(0, 1)), 0);
        assert_eq!(cur_row(&s), 1);
    }

    // ---- relative / absolute / clamp ---------------------------------

    #[test]
    fn relative_move_within_bounds() {
        let mut s = mk("hello\nworld");
        s.cursor_pos = Position::<u16>::new(2, 0);
        s.move_cursor(MoveKind::Relative(Position::new(1, 1)));
        assert_eq!(s.cursor_pos.row, 1);
        assert_eq!(s.cursor_pos.col, 3);
    }

    #[test]
    fn relative_move_clamped_at_top_left() {
        let mut s = mk("hello\nworld");
        s.move_cursor(MoveKind::Relative(Position::new(-5, -5)));
        assert_eq!(s.cursor_pos.row, 0);
        assert_eq!(s.cursor_pos.col, 0);
    }

    #[test]
    fn absolute_moves_to_file_position() {
        let mut s = mk("aaaa\nbbbb\ncccc\ndddd\neeee");
        s.cursor_pos = Position::<u16>::new(3, 2);
        s.move_cursor(MoveKind::Absolute(Position::new(0, 0)));
        assert_eq!(cur_row(&s), 0);
        assert_eq!(cur_col(&s), 0);
    }

    #[test]
    fn cur_line_returns_the_right_line() {
        let mut s = mk("ab\ncd\nef");
        s.cursor_pos = Position::<u16>::new(0, 2);
        assert_eq!(s.cur_line().to_string(), "ef");
    }

    #[test]
    fn clamp_keeps_cursor_in_buffer() {
        let mut s = mk("ab\ncd");
        s.cursor_pos = Position::<u16>::new(50, 50);
        s.clamp_cursor();
        assert_eq!(s.cursor_pos.row, 1);
        assert_eq!(s.cursor_pos.col, 1);
    }

    #[test]
    fn clamp_on_empty_buffer() {
        let mut s = mk("");
        s.cursor_pos = Position::<u16>::new(10, 10);
        s.clamp_cursor();
        assert_eq!(s.cursor_pos.row, 0);
        assert_eq!(s.cursor_pos.col, 0);
    }

    #[test]
    fn clamp_does_not_allow_landing_on_newline() {
        let mut s = mk("abc\ndef");
        s.mode = EditingMode::Insert;
        s.cursor_pos = Position::<u16>::new(10, 0);
        s.clamp_cursor();
        assert_eq!(s.cursor_pos.col, 3);
    }

    // ---- vertical scrolling -------------------------------------------

    #[test]
    fn move_down_within_viewport_does_not_scroll() {
        let mut s = mk("a\nb\nc\nd\ne");
        s.viewport.row = 3;
        s.move_cursor(MoveKind::Relative(Position::new(0, 2)));
        assert_eq!(s.cursor_pos.row, 2);
        assert_eq!(s.file_pos.row, 0);
    }

    #[test]
    fn move_down_past_viewport_scrolls_file_pos() {
        let mut s = mk("a\nb\nc\nd\ne");
        s.viewport.row = 3;
        s.move_cursor(MoveKind::Relative(Position::new(0, 4)));
        assert_eq!(s.cursor_pos.row, 2);
        assert_eq!(s.file_pos.row, 2);
        assert_eq!(cur_row(&s), 4);
    }

    #[test]
    fn move_up_past_viewport_scrolls_file_pos() {
        let mut s = mk("a\nb\nc\nd\ne");
        s.viewport.row = 2;
        s.file_pos.row = 3;
        s.move_cursor(MoveKind::Relative(Position::new(0, -1)));
        assert_eq!(s.file_pos.row, 2);
        assert_eq!(s.cursor_pos.row, 0);
    }

    #[test]
    fn file_end_scrolls_to_bottom() {
        let mut s = mk("a\nb\nc\nd\ne");
        s.viewport.row = 2;
        s.move_cursor(MoveKind::FileEnd);
        assert_eq!(s.file_pos.row, 3);
        assert_eq!(s.cursor_pos.row, 1);
    }

    #[test]
    fn file_start_resets_scroll() {
        let mut s = mk("a\nb\nc\nd\ne");
        s.viewport.row = 2;
        s.file_pos.row = 3;
        s.move_cursor(MoveKind::FileStart);
        assert_eq!(s.file_pos.row, 0);
        assert_eq!(s.cursor_pos.row, 0);
    }

    #[test]
    fn relative_up_clamps_at_top_when_already_at_origin() {
        let mut s = mk("a\nb\nc");
        s.viewport.row = 2;
        s.move_cursor(MoveKind::Relative(Position::new(0, -5)));
        assert_eq!(s.file_pos.row, 0);
        assert_eq!(s.cursor_pos.row, 0);
    }

    #[test]
    fn half_page_down_centers_cursor() {
        let mut s = mk("0\n1\n2\n3\n4\n5\n6\n7\n8\n9");
        s.viewport.row = 4;
        s.cursor_pos = Position::<u16>::new(0, 1);
        s.move_cursor(MoveKind::HalfPageDown);
        assert_eq!(s.file_pos.row, 1);
        assert_eq!(s.cursor_pos.row, 2);
        assert_eq!(cur_row(&s), 3);
    }

    #[test]
    fn half_page_up_centers_cursor() {
        let mut s = mk("0\n1\n2\n3\n4\n5\n6\n7\n8\n9");
        s.viewport.row = 4;
        s.file_pos.row = 4;
        s.cursor_pos = Position::<u16>::new(0, 2);
        s.move_cursor(MoveKind::HalfPageUp);
        assert_eq!(s.file_pos.row, 2);
        assert_eq!(s.cursor_pos.row, 2);
        assert_eq!(cur_row(&s), 4);
    }

    #[test]
    fn center_puts_cursor_in_middle_of_viewport() {
        let mut s = mk("0\n1\n2\n3\n4\n5\n6\n7\n8\n9");
        s.viewport.row = 5;
        s.file_pos.row = 6;
        s.move_cursor(MoveKind::Center);
        assert_eq!(s.file_pos.row, 4);
        assert_eq!(s.cursor_pos.row, 2);
        assert_eq!(cur_row(&s), 6);
    }

    #[test]
    fn half_page_up_at_top_does_not_scroll_past_origin() {
        let mut s = mk("0\n1\n2\n3\n4\n5");
        s.viewport.row = 4;
        s.cursor_pos = Position::<u16>::new(0, 1);
        s.move_cursor(MoveKind::HalfPageUp);
        assert_eq!(s.file_pos.row, 0);
        assert_eq!(s.cursor_pos.row, 0);
    }

    // ---- goal column (sticky col for vertical motion) ---------------

    #[test]
    fn visual_vertical_motion_preserves_goal_col_across_short_line() {
        // Lines length 8, 2, 10 — anchor col 8 on line A, stepping through
        // line B clamps to 2 but stepping onto line C restores col 8.
        let mut s = mk("12345678\nab\n0123456789");
        s.set_mode(EditingMode::Visual);
        s.move_cursor(MoveKind::LineEnd);
        assert_eq!(cur_col(&s), 8);
        s.move_cursor(MoveKind::Relative(Position::new(0, 1)));
        assert_eq!(cur_col(&s), 2);
        s.move_cursor(MoveKind::Relative(Position::new(0, 1)));
        assert_eq!(cur_col(&s), 8);
    }

    #[test]
    fn normal_mode_vertical_motion_preserves_goal_col() {
        let mut s = mk("12345678\nab\n0123456789");
        s.mode = EditingMode::Normal;
        s.cursor_pos = Position::<u16>::new(7, 0);
        s.move_cursor(MoveKind::Relative(Position::new(0, 1)));
        assert_eq!(cur_col(&s), 1);
        s.move_cursor(MoveKind::Relative(Position::new(0, 1)));
        assert_eq!(cur_col(&s), 7);
    }

    #[test]
    fn insert_mode_vertical_motion_preserves_goal_col() {
        let mut s = mk("12345678\nab\n0123456789");
        s.mode = EditingMode::Insert;
        s.cursor_pos = Position::<u16>::new(8, 0);
        s.move_cursor(MoveKind::Relative(Position::new(0, 1)));
        assert_eq!(cur_col(&s), 2);
        s.move_cursor(MoveKind::Relative(Position::new(0, 1)));
        assert_eq!(cur_col(&s), 8);
    }

    #[test]
    fn horizontal_motion_resets_goal_col() {
        let mut s = mk("12345678\nab\n0123456789");
        s.set_mode(EditingMode::Visual);
        s.move_cursor(MoveKind::LineEnd);
        s.move_cursor(MoveKind::Relative(Position::new(0, 1)));
        assert_eq!(cur_col(&s), 2);
        // LineStart re-anchors on the short line — next vertical step uses
        // col 0, not the prior goal of 8.
        s.move_cursor(MoveKind::LineStart);
        s.move_cursor(MoveKind::Relative(Position::new(0, 1)));
        assert_eq!(cur_col(&s), 0);
    }

    #[test]
    fn typing_resets_goal_col() {
        let mut s = mk("12345678\nab\n0123456789");
        s.mode = EditingMode::Insert;
        s.cursor_pos = Position::<u16>::new(8, 0);
        s.move_cursor(MoveKind::Relative(Position::new(0, 1)));
        assert_eq!(cur_col(&s), 2);
        s.insert_char('!');
        s.move_cursor(MoveKind::Relative(Position::new(0, 1)));
        assert_eq!(cur_col(&s), 3);
    }

    // ---- selected_text ------------------------------------------------

    #[test]
    fn selected_text_none_when_not_visual() {
        let s = mk("hello");
        assert_eq!(s.selected_text(), None);
    }

    #[test]
    fn selected_text_visual_forward_single_line() {
        let mut s = mk("hello");
        s.set_mode(EditingMode::Visual);
        s.cursor_pos = Position::<u16>::new(2, 0);
        assert_eq!(s.selected_text().as_deref(), Some("hel"));
    }

    #[test]
    fn selected_text_visual_line_single_line() {
        let mut s = mk("abc\ndef\nghi");
        s.cursor_pos = Position::<u16>::new(0, 1);
        s.set_mode(EditingMode::VisualLine);
        assert_eq!(s.selected_text().as_deref(), Some("def\n"));
    }

    #[test]
    fn selected_text_visual_block_rectangle() {
        let mut s = mk("abcde\nfghij\nklmno");
        s.cursor_pos = Position::<u16>::new(1, 0);
        s.set_mode(EditingMode::VisualBlock);
        s.cursor_pos = Position::<u16>::new(3, 2);
        assert_eq!(s.selected_text().as_deref(), Some("bcd\nghi\nlmn"));
    }

    // ---- undo / redo --------------------------------------------------

    #[test]
    fn undo_reverts_insert_char_and_restores_cursor() {
        let mut s = mk("ab");
        s.mode = EditingMode::Insert;
        s.cursor_pos = Position::<u16>::new(2, 0);
        s.insert_char('c');
        assert_eq!(s.buf.to_string(), "abc");
        assert_eq!(cur_col(&s), 3);
        assert!(s.undo());
        assert_eq!(s.buf.to_string(), "ab");
        assert_eq!(cur_col(&s), 2);
    }

    #[test]
    fn redo_reapplies_insert_char_and_restores_cursor() {
        let mut s = mk("ab");
        s.mode = EditingMode::Insert;
        s.cursor_pos = Position::<u16>::new(2, 0);
        s.insert_char('c');
        s.undo();
        assert!(s.redo());
        assert_eq!(s.buf.to_string(), "abc");
        assert_eq!(cur_col(&s), 3);
    }

    #[test]
    fn undo_chain_then_new_edit_drops_redo() {
        // Each insert run is coalesced into one undo step; the mode round-trip
        // closes the run so the three inserts become three distinct nodes.
        let mut s = mk("");
        s.set_mode(EditingMode::Insert);
        s.insert_char('a');
        s.set_mode(EditingMode::Normal);
        s.set_mode(EditingMode::Insert);
        s.insert_char('b');
        s.set_mode(EditingMode::Normal);
        s.set_mode(EditingMode::Insert);
        s.insert_char('c');
        s.undo();
        s.undo();
        assert_eq!(s.buf.to_string(), "a");
        assert_eq!(cur_col(&s), 1);
        s.insert_char('Z');
        assert_eq!(s.buf.to_string(), "aZ");
        assert!(!s.redo());
    }

    #[test]
    fn consecutive_inserts_coalesce_into_one_undo_step() {
        let mut s = mk("");
        s.set_mode(EditingMode::Insert);
        s.insert_char('a');
        s.insert_char('b');
        s.insert_char('c');
        assert_eq!(s.buf.to_string(), "abc");
        assert!(s.undo());
        assert_eq!(s.buf.to_string(), "");
        assert!(!s.undo());
    }

    #[test]
    fn insert_run_breaks_on_mode_change() {
        let mut s = mk("");
        s.set_mode(EditingMode::Insert);
        s.insert_char('a');
        s.insert_char('b');
        s.set_mode(EditingMode::Normal);
        s.set_mode(EditingMode::Insert);
        s.insert_char('c');
        assert!(s.undo());
        assert_eq!(s.buf.to_string(), "ab");
        assert!(s.undo());
        assert_eq!(s.buf.to_string(), "");
    }

    #[test]
    fn insert_run_breaks_on_cursor_move() {
        let mut s = mk("xy");
        s.set_mode(EditingMode::Insert);
        s.cursor_pos = Position::<u16>::new(2, 0);
        s.insert_char('a');
        s.move_cursor(MoveKind::LineStart);
        s.insert_char('b');
        assert!(s.undo());
        assert_eq!(s.buf.to_string(), "xya");
        assert!(s.undo());
        assert_eq!(s.buf.to_string(), "xy");
    }

    #[test]
    fn undo_on_fresh_buffer_is_noop() {
        let mut s = mk("hello");
        assert!(!s.undo());
        assert_eq!(s.buf.to_string(), "hello");
    }

    // ---- delete_selection --------------------------------------------

    #[test]
    fn delete_selection_visual_single_line() {
        let mut s = mk("hello");
        s.set_mode(EditingMode::Visual);
        s.cursor_pos = Position::<u16>::new(2, 0);
        assert!(s.delete_selection());
        assert_eq!(s.buf.to_string(), "lo");
        assert_eq!(s.mode, EditingMode::Normal);
        assert_eq!(cur_col(&s), 0);
        assert_eq!(cur_row(&s), 0);
    }

    #[test]
    fn delete_selection_visual_across_lines_joins() {
        let mut s = mk("abc\ndef\nghi");
        s.cursor_pos = Position::<u16>::new(1, 0);
        s.set_mode(EditingMode::Visual);
        s.cursor_pos = Position::<u16>::new(1, 1);
        assert!(s.delete_selection());
        assert_eq!(s.buf.to_string(), "af\nghi");
        assert_eq!(s.mode, EditingMode::Normal);
        assert_eq!(cur_row(&s), 0);
        assert_eq!(cur_col(&s), 1);
    }

    #[test]
    fn delete_selection_visual_undo_redo_roundtrip() {
        let mut s = mk("abc\ndef\nghi");
        s.cursor_pos = Position::<u16>::new(1, 0);
        s.set_mode(EditingMode::Visual);
        s.cursor_pos = Position::<u16>::new(1, 1);
        s.delete_selection();
        assert_eq!(s.buf.to_string(), "af\nghi");
        assert!(s.undo());
        assert_eq!(s.buf.to_string(), "abc\ndef\nghi");
        assert!(s.redo());
        assert_eq!(s.buf.to_string(), "af\nghi");
    }

    #[test]
    fn delete_selection_visual_line_middle() {
        let mut s = mk("abc\ndef\nghi");
        s.cursor_pos = Position::<u16>::new(0, 1);
        s.set_mode(EditingMode::VisualLine);
        assert!(s.delete_selection());
        assert_eq!(s.buf.to_string(), "abc\nghi");
        assert_eq!(s.mode, EditingMode::Normal);
        assert_eq!(cur_row(&s), 1);
        assert_eq!(cur_col(&s), 0);
    }

    #[test]
    fn delete_selection_visual_line_last_line_eats_preceding_newline() {
        let mut s = mk("abc\ndef");
        s.cursor_pos = Position::<u16>::new(0, 1);
        s.set_mode(EditingMode::VisualLine);
        assert!(s.delete_selection());
        assert_eq!(s.buf.to_string(), "abc");
        assert_eq!(cur_row(&s), 0);
    }

    #[test]
    fn delete_selection_visual_line_last_line_undo_redo() {
        let mut s = mk("abc\ndef");
        s.cursor_pos = Position::<u16>::new(0, 1);
        s.set_mode(EditingMode::VisualLine);
        s.delete_selection();
        assert_eq!(s.buf.to_string(), "abc");
        assert!(s.undo());
        assert_eq!(s.buf.to_string(), "abc\ndef");
        assert!(s.redo());
        assert_eq!(s.buf.to_string(), "abc");
    }

    #[test]
    fn delete_selection_visual_line_all_lines() {
        let mut s = mk("abc\ndef");
        s.set_mode(EditingMode::VisualLine);
        s.cursor_pos = Position::<u16>::new(0, 1);
        assert!(s.delete_selection());
        assert_eq!(s.buf.to_string(), "");
        assert!(s.undo());
        assert_eq!(s.buf.to_string(), "abc\ndef");
    }

    #[test]
    fn delete_selection_visual_block_rectangle() {
        let mut s = mk("abcde\nfghij\nklmno");
        s.cursor_pos = Position::<u16>::new(1, 0);
        s.set_mode(EditingMode::VisualBlock);
        s.cursor_pos = Position::<u16>::new(3, 2);
        assert!(s.delete_selection());
        assert_eq!(s.buf.to_string(), "ae\nfj\nko");
        assert_eq!(s.mode, EditingMode::Normal);
        assert_eq!(cur_row(&s), 0);
        assert_eq!(cur_col(&s), 1);
    }

    #[test]
    fn delete_selection_visual_block_undo_redo() {
        // VisualBlock is implemented as one `delete_range` call per row, so
        // a 3-row block delete is 3 undo steps.
        let mut s = mk("abcde\nfghij\nklmno");
        s.cursor_pos = Position::<u16>::new(1, 0);
        s.set_mode(EditingMode::VisualBlock);
        s.cursor_pos = Position::<u16>::new(3, 2);
        s.delete_selection();
        assert_eq!(s.buf.to_string(), "ae\nfj\nko");
        while s.undo() {}
        assert_eq!(s.buf.to_string(), "abcde\nfghij\nklmno");
        while s.redo() {}
        assert_eq!(s.buf.to_string(), "ae\nfj\nko");
    }

    // ---- delete_range ------------------------------------------------

    #[test]
    fn delete_range_within_line() {
        let mut s = mk("hello");
        assert!(s.delete_range(1, 4));
        assert_eq!(s.buf.to_string(), "ho");
        assert_eq!(cur_col(&s), 1);
    }

    #[test]
    fn delete_range_across_lines_joins() {
        let mut s = mk("abc\ndef\nghi");
        assert!(s.delete_range(1, 6));
        assert_eq!(s.buf.to_string(), "af\nghi");
        assert_eq!(cur_row(&s), 0);
        assert_eq!(cur_col(&s), 1);
    }

    #[test]
    fn delete_range_whole_line() {
        let mut s = mk("abc\ndef\nghi");
        assert!(s.delete_range(4, 8));
        assert_eq!(s.buf.to_string(), "abc\nghi");
        assert!(s.undo());
        assert_eq!(s.buf.to_string(), "abc\ndef\nghi");
    }

    #[test]
    fn delete_range_empty_is_noop() {
        let mut s = mk("hello");
        assert!(!s.delete_range(2, 2));
        assert_eq!(s.buf.to_string(), "hello");
    }

    #[test]
    fn delete_range_clamps_end_past_eof() {
        let mut s = mk("hello");
        assert!(s.delete_range(2, 999));
        assert_eq!(s.buf.to_string(), "he");
    }

    #[test]
    fn delete_selection_noop_outside_visual() {
        let mut s = mk("hello");
        assert!(!s.delete_selection());
        assert_eq!(s.buf.to_string(), "hello");
    }

    // ---- delete_line (vim `dd`) --------------------------------------

    #[test]
    fn delete_line_removes_current_line_and_newline() {
        let mut s = mk("abc\ndef\nghi");
        s.cursor_pos = Position::<u16>::new(0, 1);
        assert!(s.delete_line(1));
        assert_eq!(s.buf.to_string(), "abc\nghi");
        assert_eq!(cur_row(&s), 1);
        assert_eq!(cur_col(&s), 0);
    }

    #[test]
    fn delete_line_on_last_line_eats_preceding_newline() {
        let mut s = mk("abc\ndef");
        s.cursor_pos = Position::<u16>::new(0, 1);
        assert!(s.delete_line(1));
        assert_eq!(s.buf.to_string(), "abc");
        assert_eq!(cur_row(&s), 0);
    }

    #[test]
    fn delete_line_count_deletes_multiple_lines() {
        let mut s = mk("a\nb\nc\nd\ne");
        s.cursor_pos = Position::<u16>::new(0, 1);
        assert!(s.delete_line(3));
        assert_eq!(s.buf.to_string(), "a\ne");
        assert_eq!(cur_row(&s), 1);
    }

    #[test]
    fn delete_line_undo_restores_text() {
        let mut s = mk("abc\ndef\nghi");
        s.cursor_pos = Position::<u16>::new(0, 1);
        s.delete_line(1);
        assert!(s.undo());
        assert_eq!(s.buf.to_string(), "abc\ndef\nghi");
    }

    // ---- delete_motion (vim `d<motion>`) -----------------------------

    #[test]
    fn delete_motion_word_forward_drops_through_whitespace() {
        let mut s = mk("hello world");
        assert!(s.delete_motion(MoveKind::WordForward, 1));
        assert_eq!(s.buf.to_string(), "world");
    }

    #[test]
    fn delete_motion_word_end_includes_target_char() {
        let mut s = mk("hello world");
        assert!(s.delete_motion(MoveKind::WordEnd, 1));
        assert_eq!(s.buf.to_string(), " world");
    }

    #[test]
    fn delete_motion_line_end_stops_before_newline() {
        let mut s = mk("hello world\nrest");
        s.cursor_pos = Position::<u16>::new(6, 0);
        assert!(s.delete_motion(MoveKind::LineEnd, 1));
        assert_eq!(s.buf.to_string(), "hello \nrest");
    }

    #[test]
    fn delete_motion_left_deletes_char_to_the_left() {
        let mut s = mk("abc");
        s.cursor_pos = Position::<u16>::new(1, 0);
        assert!(s.delete_motion(MoveKind::Relative(Position::new(-1, 0)), 1));
        assert_eq!(s.buf.to_string(), "bc");
    }

    #[test]
    fn delete_motion_right_at_end_of_line_deletes_last_char() {
        let mut s = mk("abc");
        s.cursor_pos = Position::<u16>::new(2, 0);
        assert!(s.delete_motion(MoveKind::Relative(Position::new(1, 0)), 1));
        assert_eq!(s.buf.to_string(), "ab");
    }

    #[test]
    fn delete_motion_word_back_deletes_to_prev_word_start() {
        let mut s = mk("hello world");
        s.cursor_pos = Position::<u16>::new(6, 0);
        assert!(s.delete_motion(MoveKind::WordStart, 1));
        assert_eq!(s.buf.to_string(), "world");
    }

    #[test]
    fn delete_motion_down_is_linewise() {
        let mut s = mk("a\nb\nc\nd\ne");
        s.cursor_pos = Position::<u16>::new(0, 1);
        assert!(s.delete_motion(MoveKind::Relative(Position::new(0, 1)), 1));
        assert_eq!(s.buf.to_string(), "a\nd\ne");
    }

    #[test]
    fn delete_motion_file_end_is_linewise() {
        let mut s = mk("a\nb\nc\nd\ne");
        s.cursor_pos = Position::<u16>::new(0, 2);
        assert!(s.delete_motion(MoveKind::FileEnd, 1));
        assert_eq!(s.buf.to_string(), "a\nb");
    }

    #[test]
    fn delete_motion_undo_restores_text() {
        let mut s = mk("hello world");
        s.delete_motion(MoveKind::WordForward, 1);
        assert!(s.undo());
        assert_eq!(s.buf.to_string(), "hello world");
    }

    // ---- replace_char_n (vim `r<char>`) ------------------------------

    #[test]
    fn replace_char_n_swaps_char_under_cursor() {
        let mut s = mk("hello");
        s.cursor_pos = Position::<u16>::new(1, 0);
        assert!(s.replace_char_n('a', 1));
        assert_eq!(s.buf.to_string(), "hallo");
        // cursor stays on the replaced char
        assert_eq!(cur_col(&s), 1);
    }

    #[test]
    fn replace_char_n_with_count_replaces_multiple() {
        let mut s = mk("hello");
        assert!(s.replace_char_n('x', 3));
        assert_eq!(s.buf.to_string(), "xxxlo");
        // cursor lands on last replaced char
        assert_eq!(cur_col(&s), 2);
    }

    #[test]
    fn replace_char_n_count_clamps_at_line_end() {
        let mut s = mk("hi\nworld");
        // count 5 on a 2-char line only consumes 2 chars; the next line
        // is untouched.
        assert!(s.replace_char_n('z', 5));
        assert_eq!(s.buf.to_string(), "zz\nworld");
        assert_eq!(cur_col(&s), 1);
    }

    #[test]
    fn replace_char_n_on_empty_line_is_noop() {
        let mut s = mk("\nrest");
        assert!(!s.replace_char_n('x', 1));
        assert_eq!(s.buf.to_string(), "\nrest");
    }

    #[test]
    fn replace_char_n_undo_restores_original() {
        let mut s = mk("hello");
        s.replace_char_n('x', 3);
        assert!(s.undo());
        assert_eq!(s.buf.to_string(), "hello");
    }

    // ---- overwrite_char (vim Replace-mode keystroke) -----------------

    #[test]
    fn overwrite_char_replaces_and_advances() {
        let mut s = mk("hello");
        s.mode = EditingMode::Replace;
        s.overwrite_char('H');
        assert_eq!(s.buf.to_string(), "Hello");
        assert_eq!(cur_col(&s), 1);
    }

    #[test]
    fn overwrite_char_at_eol_extends_line() {
        let mut s = mk("hi");
        s.mode = EditingMode::Replace;
        s.cursor_pos = Position::<u16>::new(2, 0);
        s.overwrite_char('!');
        assert_eq!(s.buf.to_string(), "hi!");
        assert_eq!(cur_col(&s), 3);
    }

    #[test]
    fn overwrite_char_run_replaces_in_place() {
        let mut s = mk("hello");
        s.mode = EditingMode::Replace;
        s.overwrite_char('H');
        s.overwrite_char('E');
        s.overwrite_char('L');
        assert_eq!(s.buf.to_string(), "HELlo");
        assert_eq!(cur_col(&s), 3);
    }

    #[test]
    fn overwrite_char_undo_restores_original() {
        let mut s = mk("hello");
        s.mode = EditingMode::Replace;
        s.overwrite_char('X');
        assert!(s.undo());
        assert_eq!(s.buf.to_string(), "hello");
    }

    // ---- replace_backspace (vim Replace-mode `<bs>`) -----------------

    #[test]
    fn replace_backspace_restores_overwritten_char() {
        let mut s = mk("hello");
        s.set_mode(EditingMode::Replace);
        s.overwrite_char('H');
        s.overwrite_char('E');
        assert_eq!(s.buf.to_string(), "HEllo");
        assert!(s.replace_backspace());
        assert_eq!(s.buf.to_string(), "Hello");
        assert_eq!(cur_col(&s), 1);
        assert!(s.replace_backspace());
        assert_eq!(s.buf.to_string(), "hello");
        assert_eq!(cur_col(&s), 0);
    }

    #[test]
    fn replace_backspace_deletes_inserted_extension() {
        let mut s = mk("hi");
        s.set_mode(EditingMode::Insert);
        s.move_cursor_n(MoveKind::LineEnd, 1);
        s.set_mode(EditingMode::Replace);
        s.overwrite_char('!');
        s.overwrite_char('?');
        assert_eq!(s.buf.to_string(), "hi!?");
        assert!(s.replace_backspace());
        assert_eq!(s.buf.to_string(), "hi!");
        assert!(s.replace_backspace());
        assert_eq!(s.buf.to_string(), "hi");
        assert_eq!(cur_col(&s), 2);
    }

    #[test]
    fn replace_backspace_handles_overwrite_then_extension_mix() {
        let mut s = mk("hi");
        s.set_mode(EditingMode::Replace);
        s.overwrite_char('H'); // overwrite 'h'
        s.overwrite_char('I'); // overwrite 'i'
        s.overwrite_char('!'); // extends past EOL
        s.overwrite_char('?'); // extends further
        assert_eq!(s.buf.to_string(), "HI!?");
        s.replace_backspace(); // deletes '?'
        assert_eq!(s.buf.to_string(), "HI!");
        s.replace_backspace(); // deletes '!'
        assert_eq!(s.buf.to_string(), "HI");
        s.replace_backspace(); // restores 'i'
        assert_eq!(s.buf.to_string(), "Hi");
        s.replace_backspace(); // restores 'h'
        assert_eq!(s.buf.to_string(), "hi");
        assert_eq!(cur_col(&s), 0);
    }

    #[test]
    fn replace_backspace_past_session_start_is_noop() {
        let mut s = mk("hello");
        s.set_mode(EditingMode::Replace);
        s.overwrite_char('H');
        assert!(s.replace_backspace());
        // history is now empty — another bs is a no-op
        assert!(!s.replace_backspace());
        assert_eq!(s.buf.to_string(), "hello");
        assert_eq!(cur_col(&s), 0);
    }

    #[test]
    fn replace_backspace_outside_replace_mode_is_noop() {
        let mut s = mk("hello");
        // No batch active → no-op.
        assert!(!s.replace_backspace());
        assert_eq!(s.buf.to_string(), "hello");
    }

    #[test]
    fn replace_backspace_then_overwrite_records_correct_delta() {
        // Overwrite, back up, overwrite something different. The committed
        // delta should reflect the final state — not the intermediate one.
        let mut s = mk("abcde");
        s.set_mode(EditingMode::Replace);
        s.overwrite_char('X'); // replaces 'a' → "Xbcde"
        s.overwrite_char('Y'); // replaces 'b' → "XYcde"
        s.replace_backspace(); // restores 'b'  → "Xbcde"
        s.overwrite_char('Z'); // replaces 'b' → "XZcde"
        s.set_mode(EditingMode::Normal); // commits batch
        assert_eq!(s.buf.to_string(), "XZcde");
        // One undo should revert the entire session.
        assert!(s.undo());
        assert_eq!(s.buf.to_string(), "abcde");
    }

    #[test]
    fn replace_session_is_one_undo_step() {
        let mut s = mk("hello world");
        s.set_mode(EditingMode::Replace);
        s.overwrite_char('H');
        s.overwrite_char('E');
        s.overwrite_char('L');
        s.overwrite_char('L');
        s.overwrite_char('O');
        s.set_mode(EditingMode::Normal);
        assert_eq!(s.buf.to_string(), "HELLO world");
        assert!(s.undo());
        assert_eq!(s.buf.to_string(), "hello world");
        // And no more undos for that session.
        assert!(!s.undo());
    }

    #[test]
    fn replace_session_canceled_by_full_backspace_records_no_delta() {
        let mut s = mk("hello");
        s.set_mode(EditingMode::Replace);
        s.overwrite_char('A');
        s.overwrite_char('B');
        s.replace_backspace();
        s.replace_backspace();
        s.set_mode(EditingMode::Normal);
        assert_eq!(s.buf.to_string(), "hello");
        // Nothing to undo — the cancelled session didn't touch the changetree.
        assert!(!s.undo());
    }

    #[test]
    fn cursor_move_mid_replace_commits_session() {
        let mut s = mk("hello world");
        s.set_mode(EditingMode::Replace);
        s.overwrite_char('H');
        s.overwrite_char('E');
        s.move_cursor(MoveKind::Relative(Position::new(4, 0)));
        // First session should be committed now; undo restores it alone.
        assert!(s.undo());
        assert_eq!(s.buf.to_string(), "hello world");
    }
}
