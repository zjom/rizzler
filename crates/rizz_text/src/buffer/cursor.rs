//! Cursor movement and the [`MoveKind`] enum that names every flavor of
//! motion. Visual-row-aware steps consult the buffer's cached `WrapMap`;
//! anything wrap-related delegates to [`crate::scroll`].

use std::str::FromStr;

use ropey::Rope;

use rizz_core::{EditingMode, Position};

use crate::motions;
use crate::scroll;

use super::Buffer;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Copy)]
pub enum MoveKind {
    LineStart,
    /// First non-blank character on the current line (vim `^`).
    LineFirstNonBlank,
    LineEnd,
    FileStart,
    FileEnd,
    /// Vim `b` — start of the word at/before the cursor.
    WordStart,
    /// Vim `w` — start of the next word.
    WordForward,
    /// Vim `e` — end of the word at/after the cursor.
    WordEnd,
    /// Vim `ge` — end of the previous word.
    WordBackEnd,
    /// Vim `B` — start of the WORD at/before the cursor.
    BigWordStart,
    /// Vim `W` — start of the next WORD.
    BigWordForward,
    /// Vim `E` — end of the WORD at/after the cursor.
    BigWordEnd,
    /// Vim `gE` — end of the previous WORD.
    BigWordBackEnd,
    /// Vim `%` — jump to the matching bracket of `()[]{}`. If the cursor is
    /// not on a bracket, the first bracket on the current line is used.
    MatchBracket,
    Relative(Position<i16>),
    Absolute(Position<usize>),
    LineNum(usize),
    HalfPageDown,
    HalfPageUp,
    /// Vim's `zz` — re-center the viewport on the cursor without moving it.
    Center,
}

impl MoveKind {
    pub fn is_linewise_motion(&self) -> bool {
        use MoveKind as MK;
        match self {
            MK::FileStart | MK::FileEnd | MK::LineNum(_) => true,
            MK::Relative(p) if p.row != 0 => true,
            _ => false,
        }
    }

    pub fn is_inclusive_motion(&self) -> bool {
        matches!(self, MoveKind::WordEnd | MoveKind::BigWordEnd)
    }
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
            "line-first-non-blank" => M::LineFirstNonBlank,
            "line-end" => M::LineEnd,
            "file-start" => M::FileStart,
            "file-end" => M::FileEnd,
            "word-start" => M::WordStart,
            "word-forward" => M::WordForward,
            "word-end" => M::WordEnd,
            "word-back-end" => M::WordBackEnd,
            "big-word-start" => M::BigWordStart,
            "big-word-forward" => M::BigWordForward,
            "big-word-end" => M::BigWordEnd,
            "big-word-back-end" => M::BigWordBackEnd,
            "match-bracket" => M::MatchBracket,
            "half-page-down" => M::HalfPageDown,
            "half-page-up" => M::HalfPageUp,
            "center" => M::Center,
            _ => return Err("unknown MoveKind"),
        })
    }
}

impl Buffer {
    /// Apply `m` `count` times. For [`MoveKind::Relative`] the count
    /// multiplies the delta in one shot; for every other variant we just
    /// loop. `count == 0` is treated as 1 so a bare bind without a numeric
    /// prefix still works.
    pub fn move_cursor_n(&mut self, m: MoveKind, count: u32) {
        let n = count.max(1);
        if let MoveKind::Relative(Position { col, row }) = m {
            let scaled = MoveKind::Relative(Position::new(
                (col as i32)
                    .saturating_mul(n as i32)
                    .clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                (row as i32)
                    .saturating_mul(n as i32)
                    .clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            ));
            self.move_cursor(scaled);
            return;
        }
        for _ in 0..n {
            self.move_cursor(m);
        }
    }

    pub fn move_cursor(&mut self, m: MoveKind) {
        use MoveKind as MK;
        let is_pure_vertical = matches!(
            m,
            MK::Relative(Position { col: 0, row }) if row != 0
        );
        // close_insert_batch drops goal_col; capture and restore for the
        // vertical-motion case so a `j`/`k` run keeps its anchor column.
        let saved_goal = self.goal_col;
        self.close_insert_batch();
        if is_pure_vertical {
            self.goal_col = saved_goal.or(Some(self.abs_col()));
        }
        match m {
            MK::Relative(Position { col: dx, row: dy }) => {
                let abs = self.abs_pos();
                let src_col = if dy != 0 {
                    self.goal_col.unwrap_or(abs.col)
                } else {
                    abs.col
                };

                let visual_target = if dy != 0 {
                    scroll::visual_step(self.wrap_cache.as_ref(), abs.row, src_col, dy)
                } else {
                    None
                };

                let (abs_row, abs_col) = match visual_target {
                    Some((r, c)) => {
                        let c = (c as isize + dx as isize).max(0) as usize;
                        (r, c)
                    }
                    None => {
                        let r = (self.cursor_pos.row as isize)
                            .saturating_add(self.file_pos.row as isize)
                            .saturating_add(dy as isize)
                            .max(0) as usize;
                        let c = (src_col as isize).saturating_add(dx as isize).max(0) as usize;
                        (r, c)
                    }
                };

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
            MK::LineFirstNonBlank => {
                let line = self.cur_line();
                let len = line.len_chars();
                let effective = if len > 0 && line.char(len - 1) == '\n' {
                    len - 1
                } else {
                    len
                };
                let mut i = 0;
                while i < effective && line.char(i).is_ascii_whitespace() {
                    i += 1;
                }
                self.cursor_pos.col = if i == effective { 0 } else { i as u16 };
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
            MK::WordStart => self.apply_motion(motions::word_back_start, false),
            MK::WordForward => self.apply_motion(motions::word_forward, false),
            MK::WordEnd => self.apply_motion(motions::word_end, false),
            MK::WordBackEnd => self.apply_motion(motions::word_back_end, false),
            MK::BigWordStart => self.apply_motion(motions::word_back_start, true),
            MK::BigWordForward => self.apply_motion(motions::word_forward, true),
            MK::BigWordEnd => self.apply_motion(motions::word_end, true),
            MK::BigWordBackEnd => self.apply_motion(motions::word_back_end, true),
            MK::MatchBracket => self.apply_motion(motions::match_bracket, false),
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
                let abs_row = self.abs_row();
                self.center_on(abs_row);
            }
        }

        self.clamp_cursor();
    }

    /// Move the cursor by half the viewport height and re-center the
    /// viewport on the new cursor row (matches vim's C-d / C-u + zz fusion).
    fn half_page(&mut self, direction: i16) {
        if self.viewport.row == 0 {
            return;
        }
        let abs = self.abs_pos();
        let (tgt_row, tgt_col) = scroll::half_page_target(
            self.viewport.row,
            self.wrap_cache.as_ref(),
            abs.row,
            abs.col,
            direction,
        );
        self.center_on(tgt_row);
        if let Some(col) = tgt_col {
            if col < self.file_pos.col {
                self.file_pos.col = col;
            }
            self.cursor_pos.col = (col - self.file_pos.col) as u16;
        }
    }

    /// Place `abs_row` at the vertical middle of the viewport.
    fn center_on(&mut self, abs_row: usize) {
        if self.viewport.row == 0 {
            return;
        }
        self.file_pos.row = scroll::centered_top(self.viewport.row, abs_row);
        self.cursor_pos.row = (abs_row - self.file_pos.row) as u16;
    }

    pub fn clamp_cursor(&mut self) {
        let last_line = self.buf.len_lines().saturating_sub(1);
        let abs_row = self.abs_row().min(last_line);

        if self.viewport.row > 0 {
            let abs_col_now = self.abs_col();
            self.file_pos.row = scroll::clamp_scroll_top(
                self.viewport.row,
                self.wrap_cache.as_ref(),
                self.file_pos.row,
                abs_row,
                abs_col_now,
                last_line,
            );
        }
        self.cursor_pos.row = abs_row.saturating_sub(self.file_pos.row) as u16;

        let line = self.buf.line(abs_row);
        let len = line.len_chars();
        let has_trailing_nl = len > 0 && line.char(len - 1) == '\n';
        let chars = if has_trailing_nl { len - 1 } else { len };
        let max_col = match self.mode {
            EditingMode::Normal => chars.saturating_sub(1),
            EditingMode::Insert
            | EditingMode::Command
            | EditingMode::Visual
            | EditingMode::VisualLine
            | EditingMode::VisualBlock => chars,
        };
        let abs_col = self.abs_col().min(max_col);
        self.cursor_pos.col = abs_col.saturating_sub(self.file_pos.col) as u16;
    }

    /// Resolve `motion` against the rope from the cursor's current absolute
    /// char index and move there.
    fn apply_motion(&mut self, motion: fn(&Rope, usize, bool) -> usize, big: bool) {
        let abs = self.abs_pos();
        let cidx = self.buf.line_to_char(abs.row) + abs.col;
        let new = motion(&self.buf, cidx, big);
        self.set_abs_char(new);
    }

    /// Place the cursor at absolute char index `cidx` in the rope.
    fn set_abs_char(&mut self, cidx: usize) {
        let row = self.buf.char_to_line(cidx);
        let col = cidx - self.buf.line_to_char(row);
        if row < self.file_pos.row {
            self.file_pos.row = row;
        }
        self.cursor_pos.row = (row - self.file_pos.row) as u16;
        if col < self.file_pos.col {
            self.file_pos.col = col;
        }
        self.cursor_pos.col = (col - self.file_pos.col) as u16;
    }
}
