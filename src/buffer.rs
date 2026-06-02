use ropey::{Rope, RopeSlice, iter::Lines};
use std::{
    io::{self},
    path::Path,
    rc::Rc,
    str::FromStr,
};

use crate::{mode::EditingMode, position::Position};

/// What sort of buffer this is. Drives default mode and gates operations like
/// BufDelete/BufNext — the minibuffer participates in everything a file
/// buffer does but is excluded from user-visible buffer cycling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BufferKind {
    #[default]
    File,
    Minibuffer,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Copy)]
pub enum MoveKind {
    LineStart,
    LineEnd,
    FileStart,
    FileEnd,
    WordStart,
    WordEnd,
    Relative(Position<i16>),   // up, down, left, right of cursor
    Absolute(Position<usize>), // position in file
    LineNum(usize),
    HalfPageDown,
    HalfPageUp,
    /// Vim's `zz` — re-center the viewport on the cursor without moving it.
    Center,
}

impl FromStr for MoveKind {
    type Err = &'static str;
    fn from_str(sym: &str) -> Result<Self, Self::Err> {
        use MoveKind as M;
        Ok(match sym {
            "down" => M::Relative(Position::new(0, 1)),
            "up" => M::Relative(Position::new(0, -1)),
            "left" => M::Relative(Position::new(-1, 0)),
            "right" => M::Relative(Position::new(1, 0)),
            "line-start" => M::LineStart,
            "line-end" => M::LineEnd,
            "file-start" => M::FileStart,
            "file-end" => M::FileEnd,
            "word-start" => M::WordStart,
            "word-end" => M::WordEnd,
            "half-page-down" => M::HalfPageDown,
            "half-page-up" => M::HalfPageUp,
            "center" => M::Center,
            _ => return Err("unknown MoveKind"),
        })
    }
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
    pub(crate) viewport: Position<u16>,
    pub(crate) kind: BufferKind,
    pub(crate) mode: EditingMode,
    /// Anchor (absolute file position) of the current visual selection.
    /// `Some` iff `mode` is one of the visual modes — managed by `set_mode`.
    pub(crate) selection_anchor: Option<Position<usize>>,
    // pub(crate) permissions: Permissions,
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

    pub fn fs_path(&self) -> Option<Rc<Path>> {
        self.fs_path.clone()
    }

    pub fn kind(&self) -> BufferKind {
        self.kind
    }

    pub fn mode(&self) -> EditingMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: EditingMode) {
        let was_visual = self.mode.is_visual();
        let is_visual = mode.is_visual();
        if is_visual && !was_visual {
            self.selection_anchor = Some(self.abs_pos());
        } else if !is_visual {
            self.selection_anchor = None;
        }
        self.mode = mode;
    }

    /// Anchor of the current visual selection (absolute file position).
    pub fn selection_anchor(&self) -> Option<Position<usize>> {
        self.selection_anchor
    }

    /// Text covered by the current visual selection. Inclusive on both ends;
    /// `VisualLine` includes the trailing newline of the last selected row,
    /// and `VisualBlock` joins each row's column slice with `\n`.
    pub fn selected_text(&self) -> Option<String> {
        let anchor = self.selection_anchor?;
        let cursor = self.abs_pos();

        match self.mode {
            EditingMode::Visual => {
                let (start, end) = if (anchor.row, anchor.col) <= (cursor.row, cursor.col) {
                    (anchor, cursor)
                } else {
                    (cursor, anchor)
                };
                let s = self.buf.line_to_char(start.row) + start.col;
                let e = (self.buf.line_to_char(end.row) + end.col + 1).min(self.buf.len_chars());
                Some(self.buf.slice(s..e).to_string())
            }
            EditingMode::VisualLine => {
                let (lo, hi) = if anchor.row <= cursor.row {
                    (anchor.row, cursor.row)
                } else {
                    (cursor.row, anchor.row)
                };
                let s = self.buf.line_to_char(lo);
                let last_line = self.buf.len_lines().saturating_sub(1);
                let e = if hi >= last_line {
                    self.buf.len_chars()
                } else {
                    self.buf.line_to_char(hi + 1)
                };
                Some(self.buf.slice(s..e).to_string())
            }
            EditingMode::VisualBlock => {
                let (lo_row, hi_row) = if anchor.row <= cursor.row {
                    (anchor.row, cursor.row)
                } else {
                    (cursor.row, anchor.row)
                };
                let (lo_col, hi_col) = if anchor.col <= cursor.col {
                    (anchor.col, cursor.col)
                } else {
                    (cursor.col, anchor.col)
                };
                let mut out = String::new();
                for row in lo_row..=hi_row {
                    let line = self.buf.line(row);
                    let mut len = line.len_chars();
                    if len > 0 && line.char(len - 1) == '\n' {
                        len -= 1;
                    }
                    let s = lo_col.min(len);
                    let e = (hi_col + 1).min(len);
                    if s < e {
                        out.push_str(&line.slice(s..e).to_string());
                    }
                    if row != hi_row {
                        out.push('\n');
                    }
                }
                Some(out)
            }
            _ => None,
        }
    }

    /// Cursor's absolute file position (file_pos + cursor_pos).
    pub fn abs_pos(&self) -> Position<usize> {
        Position::new(
            self.file_pos.col + self.cursor_pos.col as usize,
            self.file_pos.row + self.cursor_pos.row as usize,
        )
    }

    /// Reset rope content and cursor — used when the minibuffer finishes
    /// processing a command and needs to be empty again.
    pub fn clear(&mut self) {
        self.buf = Rope::new();
        self.cursor_pos = Position::default();
        self.file_pos = Position::default();
    }

    pub fn clear_with(&mut self, text: &str) {
        self.buf = Rope::from_str(text);
        self.clamp_cursor();
    }

    /// Owned snapshot of the rope text — used by command parsing.
    pub fn text(&self) -> String {
        self.buf.to_string()
    }

    pub fn from_reader(r: impl io::Read) -> io::Result<Self> {
        Ok(Self {
            buf: Rope::from_reader(r)?,
            ..Self::default()
        })
    }

    /// Creates a new buffer with `fs_path` set to `path`.
    /// Attempts to read from path, if fails, creates empty buffer
    /// Never fails.
    pub fn with_path(path: Rc<Path>) -> Self {
        let mut buf = std::fs::File::open(&path)
            .and_then(Buffer::from_reader)
            .unwrap_or_default();
        buf.fs_path = Some(path);
        buf
    }

    /// Writes the contents of the buffer to disk
    ///
    /// Sets [Self::fs_path] to whichever path was sucessful.
    /// Priority:
    /// 1. `path` arg
    /// 2. [Self::fs_path]
    ///
    /// Noop if both are None (returns Ok)
    pub fn write(&mut self, path: Option<Rc<Path>>) -> io::Result<()> {
        // if !self.permissions.contains(Permissions::WRITE) {
        //     return Ok(());
        // }
        let resolved = path.or_else(|| self.fs_path.take());

        if let Some(path) = resolved {
            let f = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&path)?;

            self.buf.write_to(f)?;
            self.fs_path = Some(path)
        }

        Ok(())
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

    pub fn insert_char(&mut self, c: char) {
        let cidx = self.cur_line_start() + self.file_pos.col + self.cursor_pos.col as usize;
        self.buf.insert_char(cidx, c);

        if c == '\n' {
            self.cursor_pos.row = self.cursor_pos.row.saturating_add(1);
            self.cursor_pos.col = 0;
        } else {
            self.cursor_pos.col = self.cursor_pos.col.saturating_add(1);
        }
    }

    pub fn delete_char(&mut self) {
        let cidx = self.cur_line_start() + self.file_pos.col + self.cursor_pos.col as usize;
        if cidx == 0 {
            return;
        }

        match self.buf.get_char(cidx - 1) {
            Some('\n') => {
                self.cursor_pos.row = self.cursor_pos.row.saturating_sub(1);
                // length of the previous line *without* its trailing newline
                self.cursor_pos.col = self.cur_line().len_chars().saturating_sub(1) as u16;
            }
            Some(_) => self.cursor_pos.col = self.cursor_pos.col.saturating_sub(1),
            None => return,
        };

        _ = self.buf.try_remove(cidx - 1..cidx);
    }

    pub fn delete_char_at(&mut self, Position { col, row }: Position<usize>) {
        if row >= self.buf.len_lines() {
            return;
        }
        let line_start = self.buf.line_to_char(row);
        let mut line_len = self.buf.line(row).len_chars();
        if line_len > 0 && self.buf.char(line_start + line_len - 1) == '\n' {
            line_len -= 1;
        }
        if col >= line_len {
            return;
        }
        let cidx = line_start + col;
        _ = self.buf.try_remove(cidx..cidx + 1);
        self.clamp_cursor();
    }

    pub fn move_cursor(&mut self, m: MoveKind) {
        use MoveKind as MK;
        match m {
            MK::Relative(Position { col: dx, row: dy }) => {
                // Compute the absolute target. We can't use saturating_add_signed
                // on cursor_pos directly because clamping to u16::0 would erase
                // any "wanted to scroll up by N" overshoot; clamp_cursor then
                // could never observe the up-scroll intent.
                let abs_row = (self.cursor_pos.row as isize)
                    .saturating_add(self.file_pos.row as isize)
                    .saturating_add(dy as isize)
                    .max(0) as usize;
                let abs_col = (self.cursor_pos.col as isize)
                    .saturating_add(self.file_pos.col as isize)
                    .saturating_add(dx as isize)
                    .max(0) as usize;

                // Up/left scrolling lives here; down/right scrolling is left
                // to clamp_cursor, which also knows the viewport bounds.
                if abs_row < self.file_pos.row {
                    self.file_pos.row = abs_row;
                }
                self.cursor_pos.row = (abs_row - self.file_pos.row) as u16;
                if abs_col < self.file_pos.col {
                    self.file_pos.col = abs_col;
                }
                self.cursor_pos.col = (abs_col - self.file_pos.col) as u16;
            }
            MK::LineStart => {
                self.cursor_pos.col = 0;
            }
            MK::LineEnd => self.cursor_pos.col = self.cur_line().len_chars() as u16,
            MK::FileStart => {
                self.cursor_pos = Position::default();
                self.file_pos = Position::default();
            }
            MK::FileEnd => {
                let last_line = self.buf.len_lines().saturating_sub(1);
                self.file_pos.row = 0;
                self.cursor_pos.row = last_line as u16;
            }
            MK::WordStart => {
                let line = self.cur_line();
                let mut i = self.cursor_pos.col as usize;
                if i > 0 {
                    i -= 1;
                    while i > 0 && line.char(i).is_ascii_whitespace() {
                        i -= 1;
                    }
                    while i > 0 && !line.char(i - 1).is_ascii_whitespace() {
                        i -= 1;
                    }
                }
                self.cursor_pos.col = i as u16;
            }
            MK::WordEnd => {
                let line = self.cur_line();
                let len = line.len_chars();
                let effective_len = if len > 0 && line.char(len - 1) == '\n' {
                    len - 1
                } else {
                    len
                };
                let mut i = self.cursor_pos.col as usize + 1;
                while i < effective_len && line.char(i).is_ascii_whitespace() {
                    i += 1;
                }
                if i < effective_len {
                    while i + 1 < effective_len && !line.char(i + 1).is_ascii_whitespace() {
                        i += 1;
                    }
                    self.cursor_pos.col = i as u16;
                }
            }
            MK::Absolute(Position { row, col }) => {
                self.file_pos = Position::new(col, row);
                self.cursor_pos = Position::default();
            }
            MK::LineNum(n) => {
                let last_line = self.buf.len_lines().saturating_sub(1);
                self.file_pos.row = 0;
                self.cursor_pos.row = n.min(last_line) as u16;
            }
            MK::HalfPageDown => self.half_page(1),
            MK::HalfPageUp => self.half_page(-1),
            MK::Center => {
                let abs_row = self.cursor_pos.row as usize + self.file_pos.row;
                self.center_on(abs_row);
            }
        }

        self.clamp_cursor();
    }

    /// Move the cursor by half the viewport height and re-center the
    /// viewport on the new cursor row (matches vim's C-d / C-u + zz fusion).
    fn half_page(&mut self, direction: i16) {
        let vh = self.viewport.row as usize;
        if vh == 0 {
            return;
        }
        let half = ((vh / 2).max(1)) as isize * direction as isize;
        let abs_row = (self.cursor_pos.row as isize)
            .saturating_add(self.file_pos.row as isize)
            .saturating_add(half)
            .max(0) as usize;
        self.center_on(abs_row);
    }

    /// Place `abs_row` at the vertical middle of the viewport. clamp_cursor
    /// applies the EOF cap and per-line column clamping afterwards.
    fn center_on(&mut self, abs_row: usize) {
        let vh = self.viewport.row as usize;
        if vh == 0 {
            return;
        }
        let center_offset = vh / 2;
        self.file_pos.row = abs_row.saturating_sub(center_offset);
        self.cursor_pos.row = (abs_row - self.file_pos.row) as u16;
    }

    pub fn clamp_cursor(&mut self) {
        let last_line = self.buf.len_lines().saturating_sub(1);
        let abs_row = (self.cursor_pos.row as usize + self.file_pos.row).min(last_line);

        // Scroll vertically so the cursor stays inside the viewport. Skipped
        // when viewport.row is 0 (e.g. tests without a known terminal size)
        // so pre-scroll behaviour is preserved.
        if self.viewport.row > 0 {
            let vh = self.viewport.row as usize;
            if abs_row < self.file_pos.row {
                self.file_pos.row = abs_row;
            } else if abs_row >= self.file_pos.row + vh {
                self.file_pos.row = abs_row + 1 - vh;
            }
            // Pin viewport so the last file line never sits above the
            // viewport bottom — avoids drawing empty rows past EOF after
            // operations like HalfPageDown.
            let max_file_pos = (last_line + 1).saturating_sub(vh);
            self.file_pos.row = self.file_pos.row.min(max_file_pos);
        }
        self.cursor_pos.row = abs_row.saturating_sub(self.file_pos.row) as u16;

        let line = self.buf.line(abs_row);
        let len = line.len_chars();
        let has_trailing_nl = len > 0 && line.char(len - 1) == '\n';
        // Number of non-newline chars on this line.
        let chars = if has_trailing_nl { len - 1 } else { len };
        // In Normal mode the cursor sits ON a character, so it cannot move past
        // the last non-newline char. In all other modes it may sit just after.
        let max_col = match self.mode {
            EditingMode::Normal => chars.saturating_sub(1),
            EditingMode::Insert
            | EditingMode::Command
            | EditingMode::Visual
            | EditingMode::VisualLine
            | EditingMode::VisualBlock => chars,
        };
        let abs_col = (self.cursor_pos.col as usize + self.file_pos.col).min(max_col);
        self.cursor_pos.col = abs_col.saturating_sub(self.file_pos.col) as u16;
    }

    fn cur_line(&self) -> RopeSlice<'_> {
        self.buf.line(self.cur_lnum())
    }

    fn cur_lnum(&self) -> usize {
        self.cursor_pos.row as usize + self.file_pos.row
    }

    fn cur_line_start(&self) -> usize {
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
        s.cursor_pos = Position::<u16>::new(1, 1); // between 'c' and 'd'
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
        assert_eq!(s.cursor_pos.row, 0);
        assert_eq!(s.cursor_pos.col, 0);
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
        assert_eq!(s.cursor_pos.row, 0);
        assert_eq!(s.cursor_pos.col, 0);
    }

    #[test]
    fn delete_newline_at_line_start() {
        let mut s = mk("ab\ncd\nef");
        s.cursor_pos = Position::<u16>::new(0, 2); // start of "ef"
        s.delete_char();
        assert_eq!(s.buf.to_string(), "ab\ncdef");
        assert_eq!(s.cursor_pos.row, 1);
        assert_eq!(s.cursor_pos.col, 2); // after "cd"
    }

    // ---- move_cursor: LineStart / LineEnd -----------------------------

    #[test]
    fn line_start_moves_to_col_zero() {
        let mut s = mk("hello\nworld");
        s.cursor_pos = Position::<u16>::new(3, 1);
        s.move_cursor(MoveKind::LineStart);
        assert_eq!(s.cursor_pos.row, 1);
        assert_eq!(s.cursor_pos.col, 0);
    }

    #[test]
    fn line_end_does_not_land_on_newline() {
        let mut s = mk("abc\ndef");
        s.mode = EditingMode::Insert;
        s.cursor_pos = Position::<u16>::new(0, 0);
        s.move_cursor(MoveKind::LineEnd);
        assert_eq!(s.cursor_pos.row, 0);
        assert_eq!(s.cursor_pos.col, 3); // just past 'c', not on '\n'
    }
    #[test]
    fn line_end_lands_on_last_char_in_normal_mode() {
        let mut s = mk("abc\ndef");
        s.mode = EditingMode::Normal;
        s.cursor_pos = Position::<u16>::new(0, 0);
        s.move_cursor(MoveKind::LineEnd);
        assert_eq!(s.cursor_pos.row, 0);
        assert_eq!(s.cursor_pos.col, 2); // on 'c'
    }

    #[test]
    fn line_end_on_last_line_without_newline() {
        let mut s = mk("abc\ndef");
        s.cursor_pos = Position::<u16>::new(0, 1);
        s.move_cursor(MoveKind::LineEnd);
        assert_eq!(s.cursor_pos.row, 1);
        assert_eq!(s.cursor_pos.col, 2); // on 'f' (normal mode)
    }

    // ---- move_cursor: FileStart / FileEnd -----------------------------

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
        let mut s = mk("a\nb\nc"); // 3 lines, last line index = 2
        s.move_cursor(MoveKind::FileEnd);
        assert_eq!(cur_row(&s), 2);
    }

    // ---- move_cursor: LineNum -----------------------------------------

    #[test]
    fn line_num_moves_to_specified_line() {
        let mut s = mk("a\nb\nc\nd\ne");
        s.move_cursor(MoveKind::LineNum(2));
        assert_eq!(cur_row(&s), 2);
    }

    #[test]
    fn line_num_zero_moves_to_first_line() {
        let mut s = mk("a\nb\nc");
        s.cursor_pos = Position::<u16>::new(0, 2);
        s.move_cursor(MoveKind::LineNum(0));
        assert_eq!(cur_row(&s), 0);
    }

    #[test]
    fn line_num_clamps_to_last_line() {
        let mut s = mk("a\nb\nc"); // valid line indices: 0, 1, 2
        s.move_cursor(MoveKind::LineNum(100));
        assert_eq!(cur_row(&s), 2);
    }

    // ---- move_cursor: WordStart / WordEnd -----------------------------

    #[test]
    fn word_end_lands_on_last_char_of_word() {
        let mut s = mk("hello world");
        s.cursor_pos = Position::<u16>::new(0, 0);
        s.move_cursor(MoveKind::WordEnd);
        assert_eq!(s.cursor_pos.col, 4); // 'o' of "hello"
    }

    #[test]
    fn word_end_from_middle_of_word() {
        let mut s = mk("hello world");
        s.cursor_pos = Position::<u16>::new(2, 0);
        s.move_cursor(MoveKind::WordEnd);
        assert_eq!(s.cursor_pos.col, 4); // 'o' of "hello"
    }

    #[test]
    fn word_end_jumps_to_next_word_when_already_at_end() {
        let mut s = mk("hello world");
        s.cursor_pos = Position::<u16>::new(4, 0); // 'o' of "hello"
        s.move_cursor(MoveKind::WordEnd);
        assert_eq!(s.cursor_pos.col, 10); // 'd' of "world"
    }

    #[test]
    fn word_start_goes_to_previous_word_start() {
        let mut s = mk("hello world foo");
        s.cursor_pos = Position::<u16>::new(8, 0); // 'r' of "world"
        s.move_cursor(MoveKind::WordStart);
        assert_eq!(s.cursor_pos.col, 6); // 'w' of "world"
    }

    #[test]
    fn word_start_from_word_start_goes_back_one_word() {
        let mut s = mk("hello world foo");
        s.cursor_pos = Position::<u16>::new(6, 0); // 'w' of "world"
        s.move_cursor(MoveKind::WordStart);
        assert_eq!(s.cursor_pos.col, 0); // 'h' of "hello"
    }

    // ---- move_cursor: Relative / Absolute -----------------------------

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
        s.cursor_pos = Position::<u16>::new(0, 0);
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

    // ---- clamp_cursor / cur_line --------------------------------------

    #[test]
    fn cur_line_returns_the_right_line() {
        let mut s = mk("ab\ncd\nef");
        s.cursor_pos = Position::<u16>::new(0, 2);
        assert_eq!(s.cur_line().to_string(), "ef");
    }

    #[test]
    fn clamp_keeps_cursor_in_buffer() {
        let mut s = mk("ab\ncd");
        s.cursor_pos = Position::<u16>::new(50, 50); // way out of bounds
        s.clamp_cursor();
        assert_eq!(s.cursor_pos.row, 1); // last line
        assert_eq!(s.cursor_pos.col, 1); // on 'd' (normal mode clamps to last char)
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
        assert_eq!(s.cursor_pos.row, 0);
        assert_eq!(s.cursor_pos.col, 3); // past 'c', not on '\n'
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
        // Absolute row 4 should sit at the last visible viewport row.
        assert_eq!(s.cursor_pos.row, 2);
        assert_eq!(s.file_pos.row, 2);
        assert_eq!(cur_row(&s), 4);
    }

    #[test]
    fn move_up_past_viewport_scrolls_file_pos() {
        let mut s = mk("a\nb\nc\nd\ne");
        s.viewport.row = 2;
        s.file_pos.row = 3;
        s.cursor_pos = Position::<u16>::new(0, 0); // cursor at abs row 3
        s.move_cursor(MoveKind::Relative(Position::new(0, -1))); // → abs row 2
        assert_eq!(s.file_pos.row, 2);
        assert_eq!(s.cursor_pos.row, 0);
    }

    #[test]
    fn file_end_scrolls_to_bottom() {
        let mut s = mk("a\nb\nc\nd\ne"); // 5 lines, last_line = 4
        s.viewport.row = 2;
        s.move_cursor(MoveKind::FileEnd);
        // Cursor on abs row 4, viewport size 2 → top row is 3.
        assert_eq!(s.file_pos.row, 3);
        assert_eq!(s.cursor_pos.row, 1);
    }

    #[test]
    fn file_start_resets_scroll() {
        let mut s = mk("a\nb\nc\nd\ne");
        s.viewport.row = 2;
        s.file_pos.row = 3;
        s.cursor_pos = Position::<u16>::new(0, 1);
        s.move_cursor(MoveKind::FileStart);
        assert_eq!(s.file_pos.row, 0);
        assert_eq!(s.cursor_pos.row, 0);
    }

    #[test]
    fn relative_up_clamps_at_top_when_already_at_origin() {
        // Cursor at top of file, no scroll possible — stays put.
        let mut s = mk("a\nb\nc");
        s.viewport.row = 2;
        s.move_cursor(MoveKind::Relative(Position::new(0, -5)));
        assert_eq!(s.file_pos.row, 0);
        assert_eq!(s.cursor_pos.row, 0);
    }

    // ---- HalfPageDown / HalfPageUp ------------------------------------

    #[test]
    fn half_page_down_centers_cursor() {
        let mut s = mk("0\n1\n2\n3\n4\n5\n6\n7\n8\n9");
        s.viewport.row = 4; // half = 2, center offset = 2
        s.cursor_pos = Position::<u16>::new(0, 1); // abs row 1
        s.move_cursor(MoveKind::HalfPageDown);
        // abs row → 3, centered: file_pos = 3 - 2 = 1, cursor at row 2.
        assert_eq!(s.file_pos.row, 1);
        assert_eq!(s.cursor_pos.row, 2);
        assert_eq!(cur_row(&s), 3);
    }

    #[test]
    fn half_page_up_centers_cursor() {
        let mut s = mk("0\n1\n2\n3\n4\n5\n6\n7\n8\n9");
        s.viewport.row = 4;
        s.file_pos.row = 4;
        s.cursor_pos = Position::<u16>::new(0, 2); // abs row 6
        s.move_cursor(MoveKind::HalfPageUp);
        // abs row → 4, centered: file_pos = 2, cursor at row 2.
        assert_eq!(s.file_pos.row, 2);
        assert_eq!(s.cursor_pos.row, 2);
        assert_eq!(cur_row(&s), 4);
    }

    // ---- Center (zz) --------------------------------------------------

    #[test]
    fn center_puts_cursor_in_middle_of_viewport() {
        let mut s = mk("0\n1\n2\n3\n4\n5\n6\n7\n8\n9");
        s.viewport.row = 5; // center offset = 2
        s.cursor_pos = Position::<u16>::new(0, 0);
        s.file_pos.row = 6; // abs row 6
        s.move_cursor(MoveKind::Center);
        assert_eq!(s.file_pos.row, 4);
        assert_eq!(s.cursor_pos.row, 2);
        assert_eq!(cur_row(&s), 6);
    }

    #[test]
    fn center_near_top_does_not_scroll_past_origin() {
        let mut s = mk("0\n1\n2\n3\n4\n5\n6\n7\n8\n9");
        s.viewport.row = 5;
        s.cursor_pos = Position::<u16>::new(0, 1); // abs row 1
        s.move_cursor(MoveKind::Center);
        // Want file_pos = 1 - 2 = -1 → clamped to 0; cursor stays at abs row 1.
        assert_eq!(s.file_pos.row, 0);
        assert_eq!(s.cursor_pos.row, 1);
    }

    #[test]
    fn center_near_eof_pins_viewport_to_last_line() {
        let mut s = mk("0\n1\n2\n3\n4\n5\n6\n7\n8\n9");
        s.viewport.row = 4; // max_file_pos = 6
        s.cursor_pos = Position::<u16>::new(0, 0);
        s.file_pos.row = 9; // abs row 9
        s.move_cursor(MoveKind::Center);
        // Center wants file_pos = 9 - 2 = 7, EOF cap pulls it back to 6.
        assert_eq!(s.file_pos.row, 6);
        assert_eq!(cur_row(&s), 9);
    }

    #[test]
    fn half_page_down_near_eof_pins_viewport_to_last_line() {
        // 10 lines (indices 0..=9), viewport 4, half = 2. From the bottom,
        // C-d shouldn't scroll past EOF (the last line stays in view).
        let mut s = mk("0\n1\n2\n3\n4\n5\n6\n7\n8\n9");
        s.viewport.row = 4;
        s.file_pos.row = 6;
        s.cursor_pos = Position::<u16>::new(0, 3); // abs row 9
        s.move_cursor(MoveKind::HalfPageDown);
        // Already at last line; viewport stays pinned with last_line at bottom.
        assert_eq!(s.file_pos.row, 6);
        assert_eq!(cur_row(&s), 9);
    }

    // ---- selected_text ------------------------------------------------

    #[test]
    fn selected_text_none_when_not_visual() {
        let s = mk("hello");
        assert_eq!(s.selected_text(), None);
    }

    #[test]
    fn selected_text_none_in_visual_without_anchor() {
        // Anchor is normally set by set_mode, but a directly-set Visual mode
        // with no anchor (e.g. mid-construction) must still return None.
        let mut s = mk("hello");
        s.mode = EditingMode::Visual;
        assert_eq!(s.selected_text(), None);
    }

    #[test]
    fn selected_text_visual_forward_single_line() {
        let mut s = mk("hello");
        s.set_mode(EditingMode::Visual); // anchor at (0,0)
        s.cursor_pos = Position::<u16>::new(2, 0);
        assert_eq!(s.selected_text().as_deref(), Some("hel"));
    }

    #[test]
    fn selected_text_visual_reverse_single_line() {
        let mut s = mk("hello");
        s.cursor_pos = Position::<u16>::new(3, 0);
        s.set_mode(EditingMode::Visual); // anchor at col 3
        s.cursor_pos = Position::<u16>::new(1, 0);
        assert_eq!(s.selected_text().as_deref(), Some("ell"));
    }

    #[test]
    fn selected_text_visual_single_char() {
        let mut s = mk("hello");
        s.set_mode(EditingMode::Visual);
        assert_eq!(s.selected_text().as_deref(), Some("h"));
    }

    #[test]
    fn selected_text_visual_multiline() {
        let mut s = mk("abc\ndef\nghi");
        s.cursor_pos = Position::<u16>::new(1, 0);
        s.set_mode(EditingMode::Visual); // anchor at (col=1, row=0)
        s.cursor_pos = Position::<u16>::new(1, 1);
        assert_eq!(s.selected_text().as_deref(), Some("bc\nde"));
    }

    #[test]
    fn selected_text_visual_clamps_at_eof() {
        let mut s = mk("ab");
        s.set_mode(EditingMode::Visual);
        s.cursor_pos = Position::<u16>::new(50, 0); // past end, no clamp called
        assert_eq!(s.selected_text().as_deref(), Some("ab"));
    }

    #[test]
    fn selected_text_visual_line_single_line() {
        let mut s = mk("abc\ndef\nghi");
        s.cursor_pos = Position::<u16>::new(0, 1);
        s.set_mode(EditingMode::VisualLine);
        assert_eq!(s.selected_text().as_deref(), Some("def\n"));
    }

    #[test]
    fn selected_text_visual_line_multiline() {
        let mut s = mk("abc\ndef\nghi");
        s.set_mode(EditingMode::VisualLine);
        s.cursor_pos = Position::<u16>::new(0, 1);
        assert_eq!(s.selected_text().as_deref(), Some("abc\ndef\n"));
    }

    #[test]
    fn selected_text_visual_line_reverse() {
        // Anchor below cursor — start/end rows must swap.
        let mut s = mk("abc\ndef\nghi");
        s.cursor_pos = Position::<u16>::new(0, 2);
        s.set_mode(EditingMode::VisualLine);
        s.cursor_pos = Position::<u16>::new(0, 0);
        assert_eq!(s.selected_text().as_deref(), Some("abc\ndef\nghi"));
    }

    #[test]
    fn selected_text_visual_line_includes_last_line_without_newline() {
        let mut s = mk("abc\ndef");
        s.set_mode(EditingMode::VisualLine);
        s.cursor_pos = Position::<u16>::new(0, 1);
        assert_eq!(s.selected_text().as_deref(), Some("abc\ndef"));
    }

    #[test]
    fn selected_text_visual_block_rectangle() {
        let mut s = mk("abcde\nfghij\nklmno");
        s.cursor_pos = Position::<u16>::new(1, 0);
        s.set_mode(EditingMode::VisualBlock);
        s.cursor_pos = Position::<u16>::new(3, 2);
        assert_eq!(s.selected_text().as_deref(), Some("bcd\nghi\nlmn"));
    }

    #[test]
    fn selected_text_visual_block_reverse_columns() {
        let mut s = mk("abcde\nfghij");
        s.cursor_pos = Position::<u16>::new(3, 0);
        s.set_mode(EditingMode::VisualBlock);
        s.cursor_pos = Position::<u16>::new(1, 1);
        assert_eq!(s.selected_text().as_deref(), Some("bcd\nghi"));
    }

    #[test]
    fn selected_text_visual_block_truncates_short_lines() {
        let mut s = mk("ab\nfghij\nk");
        s.cursor_pos = Position::<u16>::new(1, 0);
        s.set_mode(EditingMode::VisualBlock);
        s.cursor_pos = Position::<u16>::new(3, 2);
        // row 0 "ab"     → cols 1..3 capped to len 2 → "b"
        // row 1 "fghij"  → cols 1..4 → "ghi"
        // row 2 "k"      → cols capped to len 1, empty slice
        // Trailing newline after row 1 is emitted before the empty row 2.
        assert_eq!(s.selected_text().as_deref(), Some("b\nghi\n"));
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
}
