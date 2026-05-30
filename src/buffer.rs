use ropey::{Rope, RopeSlice, iter::Lines};
use std::{io, path::PathBuf, str::FromStr};

use crate::position::Position;

#[derive(Debug, Clone, Default)]
pub struct Buffer {
    pub(crate) buf: Rope,
    pub(crate) cursor_pos: Position<u16>,
    pub(crate) file_pos: Position<usize>,
    pub(crate) fs_path: Option<PathBuf>,
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
    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut buf = std::fs::File::open(&path)
            .and_then(Buffer::from_reader)
            .unwrap_or_default();
        buf.fs_path = Some(path);
        buf
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

    pub fn move_cursor(&mut self, dx: i16, dy: i16) {
        self.cursor_pos.col = self.cursor_pos.col.saturating_add_signed(dx);
        self.cursor_pos.row = self.cursor_pos.row.saturating_add_signed(dy);
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

    #[test]
    fn delete_newline_at_line_start() {
        let mut s = mk("ab\ncd\nef");
        s.cursor_pos = Position::<u16>::new(0, 2); // start of "ef"
        s.delete_char();
        assert_eq!(s.buf.to_string(), "ab\ncdef");
        assert_eq!(s.cursor_pos.row, 1);
        assert_eq!(s.cursor_pos.col, 2); // after "cd"
    }

    #[test]
    fn insert_on_second_line_uses_correct_offset() {
        let mut s = mk("ab\ncd");
        s.cursor_pos = Position::<u16>::new(1, 1); // between 'c' and 'd'
        s.insert_char('X');
        assert_eq!(s.buf.to_string(), "ab\ncXd");
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
        s.cursor_pos = Position::<u16>::new(50, 50); // way out of bounds
        s.clamp_cursor();
        assert_eq!(s.cursor_pos.row, 1); // last line
        assert_eq!(s.cursor_pos.col, 2); // end of "cd"
    }
}
