//! Non-mutating yank helpers + a kind-aware paste.
//!
//! Yank methods peek at the same ranges the corresponding delete methods
//! would touch, but they only return the text + a [`RegisterKind`] tag so
//! `State` can route the result through `rizz_registers::Registers` and
//! then (for cut-style commands) call the real delete.
//!
//! Paste consumes a register entry's kind to decide where the text lands:
//! * `Char` — inline at (or just after) the cursor.
//! * `Line` — opens a new line above/below the current row.
//! * `Block` — treated as `Char` for now (a future change can wire up
//!   column-wise insertion).

use rizz_core::{EditingMode, Position};
use rizz_registers::{RegisterEntry, RegisterKind};

use super::{Buffer, MoveKind};

impl Buffer {
    /// Text covered by the current visual selection, paired with the kind
    /// implied by the active visual sub-mode. Returns `None` when the buffer
    /// is not in a visual mode (matches [`Self::selected_text`]).
    pub fn yank_selection(&self) -> Option<(String, RegisterKind)> {
        let text = self.selected_text()?;
        let kind = match self.mode {
            EditingMode::VisualLine => RegisterKind::Line,
            EditingMode::VisualBlock => RegisterKind::Block,
            _ => RegisterKind::Char,
        };
        Some((text, kind))
    }

    /// Linewise yank of `count` whole lines starting at the cursor row
    /// (vim `yy`/`Nyy`). The returned text always ends in a newline so paste
    /// can rely on the linewise invariant.
    pub fn yank_line(&self, count: u32) -> Option<(String, RegisterKind)> {
        let n = count.max(1) as usize;
        let total = self.buf.len_lines();
        if total == 0 {
            return None;
        }
        let last_line = total.saturating_sub(1);
        let start_row = self.abs_row().min(last_line);
        let end_row = (start_row + n - 1).min(last_line);
        let s = self.buf.line_to_char(start_row);
        let e = if end_row + 1 >= total {
            self.buf.len_chars()
        } else {
            self.buf.line_to_char(end_row + 1)
        };
        if s >= e {
            return None;
        }
        let mut text = self.buf.slice(s..e).to_string();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        Some((text, RegisterKind::Line))
    }

    /// Yank from the cursor to `kind`'s destination (vim `y<motion>`).
    /// Vertical / whole-file motions yank whole lines; everything else
    /// yields a character range. Mirrors [`Self::delete_motion`]'s rules so
    /// `y<m>` and `d<m>` agree on the spanned text.
    pub fn yank_motion(&self, kind: MoveKind, count: u32) -> Option<(String, RegisterKind)> {
        let start_pos = self.abs_pos();
        let start_cidx = self.buf.line_to_char(start_pos.row) + start_pos.col;

        let mut probe = self.clone();
        probe.mode = EditingMode::Insert;
        probe.move_cursor_n(kind, count);
        let end_pos = probe.abs_pos();
        let end_cidx = probe.buf.line_to_char(end_pos.row) + end_pos.col;

        if kind.is_linewise_motion() {
            let lo = start_pos.row.min(end_pos.row);
            let hi = start_pos.row.max(end_pos.row);
            let count = (hi - lo + 1) as u32;
            let mut probe = self.clone();
            probe.file_pos = Position::new(0, lo);
            probe.cursor_pos = Position::default();
            return probe.yank_line(count);
        }

        let len = self.buf.len_chars();
        let (s, e) = if start_cidx <= end_cidx {
            let end = if kind.is_inclusive_motion() {
                (end_cidx + 1).min(len)
            } else {
                end_cidx
            };
            (start_cidx, end)
        } else {
            (end_cidx, start_cidx)
        };
        if s >= e {
            return None;
        }
        Some((self.buf.slice(s..e).to_string(), RegisterKind::Char))
    }

    /// Insert `entry`'s text at the cursor (vim `p`/`P`). Linewise entries
    /// open a new line above (`before=true`) or below (`before=false`) the
    /// current row; charwise entries land inline, just after the cursor
    /// when `before=false` and at the cursor when `before=true`. The whole
    /// paste is one undo step.
    pub fn paste(&mut self, entry: &RegisterEntry, before: bool) -> bool {
        if entry.text.is_empty() {
            return false;
        }
        match entry.kind {
            RegisterKind::Line => self.paste_linewise(&entry.text, before),
            RegisterKind::Char | RegisterKind::Block => self.paste_charwise(&entry.text, before),
        }
    }

    fn paste_charwise(&mut self, text: &str, before: bool) -> bool {
        if !before {
            let line = self.cur_line();
            let mut line_len = line.len_chars();
            if line_len > 0 && line.char(line_len - 1) == '\n' {
                line_len -= 1;
            }
            let abs_col = self.abs_col();
            if abs_col < line_len {
                let saved_mode = self.mode;
                self.mode = EditingMode::Insert;
                self.move_cursor(MoveKind::Relative(Position::new(1, 0)));
                self.mode = saved_mode;
            }
        }
        self.insert_many(text);
        true
    }

    fn paste_linewise(&mut self, text: &str, before: bool) -> bool {
        let saved_mode = self.mode;
        self.mode = EditingMode::Insert;
        let row = self.cur_lnum();

        if before {
            self.land_cursor_at(row, 0);
            self.insert_many(text);
            self.mode = saved_mode;
            self.land_cursor_at(row, 0);
            return true;
        }

        let last_line = self.buf.len_lines().saturating_sub(1);
        if row >= last_line {
            // Last line — append a newline before the payload and strip its
            // trailing newline so the file doesn't gain a dangling blank line.
            let line = self.buf.line(row);
            let mut line_len = line.len_chars();
            if line_len > 0 && line.char(line_len - 1) == '\n' {
                line_len -= 1;
            }
            self.land_cursor_at(row, line_len);
            let trimmed = text.strip_suffix('\n').unwrap_or(text);
            let mut payload = String::with_capacity(trimmed.len() + 1);
            payload.push('\n');
            payload.push_str(trimmed);
            self.insert_many(&payload);
            self.mode = saved_mode;
            self.land_cursor_at(row + 1, 0);
            return true;
        }

        self.land_cursor_at(row + 1, 0);
        self.insert_many(text);
        self.mode = saved_mode;
        self.land_cursor_at(row + 1, 0);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn mk(text: &str) -> Buffer {
        Buffer::from_str(text).unwrap()
    }

    #[test]
    fn yank_line_includes_trailing_newline() {
        let s = mk("abc\ndef\n");
        let (text, kind) = s.yank_line(1).unwrap();
        assert_eq!(text, "abc\n");
        assert_eq!(kind, RegisterKind::Line);
    }

    #[test]
    fn yank_line_synthesizes_trailing_newline_on_last_line() {
        let mut s = mk("abc\ndef");
        s.cursor_pos = Position::new(0, 1);
        let (text, _) = s.yank_line(1).unwrap();
        assert_eq!(text, "def\n");
    }

    #[test]
    fn yank_line_count_spans_multiple_lines() {
        let s = mk("a\nb\nc\nd");
        let (text, _) = s.yank_line(3).unwrap();
        assert_eq!(text, "a\nb\nc\n");
    }

    #[test]
    fn yank_motion_word_forward_is_charwise() {
        let s = mk("hello world");
        let (text, kind) = s.yank_motion(MoveKind::WordForward, 1).unwrap();
        assert_eq!(text, "hello ");
        assert_eq!(kind, RegisterKind::Char);
    }

    #[test]
    fn yank_motion_word_end_includes_target() {
        let s = mk("hello world");
        let (text, _) = s.yank_motion(MoveKind::WordEnd, 1).unwrap();
        assert_eq!(text, "hello");
    }

    #[test]
    fn yank_motion_down_is_linewise() {
        let s = mk("aaa\nbbb\nccc\nddd");
        let (text, kind) = s
            .yank_motion(MoveKind::Relative(Position::new(0, 1)), 1)
            .unwrap();
        assert_eq!(kind, RegisterKind::Line);
        assert_eq!(text, "aaa\nbbb\n");
    }

    #[test]
    fn yank_selection_visual_charwise() {
        let mut s = mk("hello");
        s.set_mode(EditingMode::Visual);
        s.cursor_pos = Position::new(2, 0);
        let (text, kind) = s.yank_selection().unwrap();
        assert_eq!(text, "hel");
        assert_eq!(kind, RegisterKind::Char);
    }

    #[test]
    fn yank_selection_visual_line() {
        let mut s = mk("abc\ndef\nghi");
        s.cursor_pos = Position::new(0, 1);
        s.set_mode(EditingMode::VisualLine);
        let (text, kind) = s.yank_selection().unwrap();
        assert_eq!(text, "def\n");
        assert_eq!(kind, RegisterKind::Line);
    }

    #[test]
    fn paste_charwise_after_inserts_after_cursor() {
        let mut s = mk("abc");
        s.cursor_pos = Position::new(0, 0);
        s.paste(&RegisterEntry::charwise("XY"), false);
        assert_eq!(s.text(), "aXYbc");
    }

    #[test]
    fn paste_charwise_before_inserts_at_cursor() {
        let mut s = mk("abc");
        s.cursor_pos = Position::new(1, 0);
        s.paste(&RegisterEntry::charwise("XY"), true);
        assert_eq!(s.text(), "aXYbc");
    }

    #[test]
    fn paste_linewise_after_opens_line_below() {
        let mut s = mk("abc\ndef\nghi");
        s.cursor_pos = Position::new(0, 1);
        s.paste(&RegisterEntry::linewise("XX\n"), false);
        assert_eq!(s.text(), "abc\ndef\nXX\nghi");
    }

    #[test]
    fn paste_linewise_before_opens_line_above() {
        let mut s = mk("abc\ndef\nghi");
        s.cursor_pos = Position::new(0, 1);
        s.paste(&RegisterEntry::linewise("XX\n"), true);
        assert_eq!(s.text(), "abc\nXX\ndef\nghi");
    }

    #[test]
    fn paste_linewise_after_on_last_line_appends() {
        let mut s = mk("abc\ndef");
        s.cursor_pos = Position::new(0, 1);
        s.paste(&RegisterEntry::linewise("XX\n"), false);
        assert_eq!(s.text(), "abc\ndef\nXX");
    }
}
