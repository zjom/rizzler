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

use std::{path::Path, rc::Rc, str::FromStr};

use rizz_changetree::ChangeTree;
use ropey::{Rope, RopeSlice, iter::Lines};

use rizz_core::{EditingMode, Position};

use crate::{
    props::PropStore,
    wrap::{WrapMap, WrapMode, WrapSettings},
};

pub use cursor::MoveKind;

/// What sort of buffer this is. Drives default mode and gates operations like
/// BufDelete/BufNext — the minibuffer participates in everything a file
/// buffer does but is excluded from user-visible buffer cycling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BufferKind {
    #[default]
    File,
    Minibuffer,
    /// Backing buffer of a popup. Excluded from user-visible buffer cycling
    /// and from `BufDelete`.
    Popup,
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
    pub(crate) kind: BufferKind,
    pub(crate) mode: EditingMode,
    /// Stack of additional keymap modes layered on top of `mode`. Used to
    /// give a buffer extra named modes (e.g. a popup buffer activating
    /// `"popup"` and `"popup.files"`) without losing its base editing mode.
    /// Last element is the most recently pushed and shadows earlier layers
    /// during keymap resolution.
    pub(crate) mode_layers: Vec<Rc<str>>,
    /// Anchor (absolute file position) of the current visual selection.
    /// `Some` iff `mode` is one of the visual modes — managed by `set_mode`.
    pub(crate) selection_anchor: Option<Position<usize>>,
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
}

impl Buffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct the editor's minibuffer — single-line, starts in Command mode,
    /// used as the destination for `:`-style command input.
    pub fn minibuffer() -> Self {
        Self {
            kind: BufferKind::Minibuffer,
            mode: EditingMode::Command,
            ..Self::default()
        }
    }

    /// Construct a popup's backing buffer. Same as a default buffer, just
    /// tagged so cycling/deletion code can skip it.
    pub fn popup() -> Self {
        Self {
            kind: BufferKind::Popup,
            ..Self::default()
        }
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

    pub fn kind(&self) -> BufferKind {
        self.kind
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
    }

    pub fn clear_with(&mut self, text: &str) {
        self.buf = Rope::from_str(text);
        self.invalidate_wrap_cache();
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
        let mut s = mk("");
        s.mode = EditingMode::Insert;
        s.insert_char('a');
        s.insert_char('b');
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
}
