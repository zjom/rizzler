use ropey::{Rope, RopeSlice, iter::Lines};
use std::{
    io::{self},
    path::Path,
    rc::Rc,
    str::FromStr,
};

use crate::{action::MoveKind, position::Position};

#[derive(Debug, Clone, Default)]
pub struct Buffer {
    pub(crate) buf: Rope,
    pub(crate) cursor_pos: Position<u16>,
    pub(crate) file_pos: Position<usize>,
    pub(crate) fs_path: Option<Rc<Path>>,
}

impl Buffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_reader(r: impl io::Read) -> io::Result<Self> {
        Ok(Self {
            buf: Rope::from_reader(r)?,
            cursor_pos: Position::default(),
            file_pos: Position::default(),
            fs_path: None,
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

    /// Writes the contents of the buffer to disk.
    ///
    /// Sets [Self::fs_path] to whichever path was sucessful.
    /// Priority:
    /// 1. `path` arg
    /// 2. [Self::fs_path]
    ///
    /// Noop if both are None (returns Ok)
    pub fn write(&mut self, path: Option<Rc<Path>>) -> io::Result<()> {
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

    pub fn move_cursor(&mut self, m: MoveKind) {
        use MoveKind as MK;
        match m {
            MK::Relative(Position { col: dx, row: dy }) => {
                self.cursor_pos.col = self.cursor_pos.col.saturating_add_signed(dx);
                self.cursor_pos.row = self.cursor_pos.row.saturating_add_signed(dy);
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
        }

        self.clamp_cursor();
    }

    pub fn clamp_cursor(&mut self) {
        let last_line = self.buf.len_lines().saturating_sub(1);
        let abs_row = (self.cursor_pos.row as usize + self.file_pos.row).min(last_line);
        self.cursor_pos.row = abs_row.saturating_sub(self.file_pos.row) as u16;

        let line = self.buf.line(abs_row);
        let mut line_len = line.len_chars();
        // don't count the trailing newline as a landable column
        if line_len > 0 && line.char(line_len - 1) == '\n' {
            line_len -= 1;
        }
        let abs_col = (self.cursor_pos.col as usize + self.file_pos.col).min(line_len);
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
            cursor_pos: Position::default(),
            file_pos: Position::default(),
            fs_path: None,
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
        s.cursor_pos = Position::<u16>::new(0, 0);
        s.move_cursor(MoveKind::LineEnd);
        assert_eq!(s.cursor_pos.row, 0);
        assert_eq!(s.cursor_pos.col, 3); // just past 'c', not on '\n'
    }

    #[test]
    fn line_end_on_last_line_without_newline() {
        let mut s = mk("abc\ndef");
        s.cursor_pos = Position::<u16>::new(0, 1);
        s.move_cursor(MoveKind::LineEnd);
        assert_eq!(s.cursor_pos.row, 1);
        assert_eq!(s.cursor_pos.col, 3);
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
        assert_eq!(s.cursor_pos.col, 2); // end of "cd"
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
        s.cursor_pos = Position::<u16>::new(10, 0);
        s.clamp_cursor();
        assert_eq!(s.cursor_pos.row, 0);
        assert_eq!(s.cursor_pos.col, 3); // past 'c', not on '\n'
    }
}
