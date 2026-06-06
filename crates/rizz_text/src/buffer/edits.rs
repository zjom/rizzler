//! Buffer-mutating edits — text insertion, deletion, and undo/redo. Each
//! mutator records a [`rizz_changetree::Delta`] so undo/redo restore both
//! the rope contents and the cursor position.

use rizz_changetree::Delta;
use rizz_core::Position;
use ropey::Rope;

use super::Buffer;

impl Buffer {
    pub fn insert_char(&mut self, c: char) {
        let cidx = self.cur_line_start() + self.file_pos.col + self.cursor_pos.col as usize;
        let start_line = self.abs_row();
        let abs_before = self.abs_pos();
        let before = snapshot_lines(&self.buf, start_line, 1);

        self.buf.insert_char(cidx, c);
        self.invalidate_wrap_cache();

        if c == '\n' {
            self.cursor_pos.row = self.cursor_pos.row.saturating_add(1);
            self.cursor_pos.col = 0;
        } else {
            self.cursor_pos.col = self.cursor_pos.col.saturating_add(1);
        }

        let after_lines = if c == '\n' { 2 } else { 1 };
        let after = snapshot_lines(&self.buf, start_line, after_lines);
        let abs_after = self.abs_pos();
        self.changetree.track_change(Delta {
            start_line,
            before: before.into(),
            after: after.into(),
            cursor_before: (abs_before.row, abs_before.col),
            cursor_after: (abs_after.row, abs_after.col),
        });
    }

    pub fn delete_char(&mut self) {
        let cidx = self.cur_line_start() + self.file_pos.col + self.cursor_pos.col as usize;
        if cidx == 0 {
            return;
        }

        // Snapshot before any rope mutation. Joining lines spans two lines;
        // an in-line delete spans one.
        let removed_is_nl = matches!(self.buf.get_char(cidx - 1), Some('\n'));
        let start_line = if removed_is_nl {
            self.abs_row().saturating_sub(1)
        } else {
            self.abs_row()
        };
        let before_lines = if removed_is_nl { 2 } else { 1 };
        let before = snapshot_lines(&self.buf, start_line, before_lines);
        let abs_before = self.abs_pos();

        match self.buf.get_char(cidx - 1) {
            Some('\n') => {
                self.cursor_pos.row = self.cursor_pos.row.saturating_sub(1);
                self.cursor_pos.col = self.cur_line().len_chars().saturating_sub(1) as u16;
            }
            Some(_) => self.cursor_pos.col = self.cursor_pos.col.saturating_sub(1),
            None => return,
        };

        _ = self.buf.try_remove(cidx - 1..cidx);
        self.invalidate_wrap_cache();

        let after = snapshot_lines(&self.buf, start_line, 1);
        let abs_after = self.abs_pos();
        self.changetree.track_change(Delta {
            start_line,
            before: before.into(),
            after: after.into(),
            cursor_before: (abs_before.row, abs_before.col),
            cursor_after: (abs_after.row, abs_after.col),
        });
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
        let before = snapshot_lines(&self.buf, row, 1);
        let abs_before = self.abs_pos();
        let cidx = line_start + col;
        _ = self.buf.try_remove(cidx..cidx + 1);
        self.invalidate_wrap_cache();
        self.clamp_cursor();
        let after = snapshot_lines(&self.buf, row, 1);
        let abs_after = self.abs_pos();
        self.changetree.track_change(Delta {
            start_line: row,
            before: before.into(),
            after: after.into(),
            cursor_before: (abs_before.row, abs_before.col),
            cursor_after: (abs_after.row, abs_after.col),
        });
    }

    /// Reverse the most recent tracked edit and return whether anything
    /// happened. Cursor lands where it was just before that edit.
    pub fn undo(&mut self) -> bool {
        let Some(delta) = self.changetree.undo() else {
            return false;
        };
        let after_lines = rope_line_count(&delta.after);
        replace_lines(&mut self.buf, delta.start_line, after_lines, &delta.before);
        self.invalidate_wrap_cache();
        let (row, col) = delta.cursor_before;
        self.land_cursor_at(row, col);
        true
    }

    /// Reapply the most recently undone edit, if any. Cursor lands where it
    /// ended up the first time around.
    pub fn redo(&mut self) -> bool {
        let Some(delta) = self.changetree.redo() else {
            return false;
        };
        let before_lines = rope_line_count(&delta.before);
        replace_lines(&mut self.buf, delta.start_line, before_lines, &delta.after);
        self.invalidate_wrap_cache();
        let (row, col) = delta.cursor_after;
        self.land_cursor_at(row, col);
        true
    }

    pub(super) fn land_cursor_at(&mut self, row: usize, col: usize) {
        let row = row.min(self.buf.len_lines().saturating_sub(1));
        if row < self.file_pos.row {
            self.file_pos.row = row;
        }
        self.cursor_pos.row = (row - self.file_pos.row) as u16;
        if col < self.file_pos.col {
            self.file_pos.col = col;
        }
        self.cursor_pos.col = (col - self.file_pos.col) as u16;
        self.clamp_cursor();
    }
}

/// Read lines `[start..start+n_lines)` as a single string. Caps at EOF so the
/// snapshot is well-defined even when the requested range overshoots.
fn snapshot_lines(rope: &Rope, start: usize, n_lines: usize) -> String {
    let total = rope.len_lines();
    if start >= total {
        return String::new();
    }
    let start_char = rope.line_to_char(start);
    let end_line = (start + n_lines).min(total);
    let end_char = if end_line >= total {
        rope.len_chars()
    } else {
        rope.line_to_char(end_line)
    };
    rope.slice(start_char..end_char).to_string()
}

/// Replace lines `[start..start+n_lines)` with `text`. Used by undo/redo to
/// swap in the opposite snapshot of a recorded delta.
fn replace_lines(rope: &mut Rope, start: usize, n_lines: usize, text: &str) {
    let total = rope.len_lines();
    let start_char = if start >= total {
        rope.len_chars()
    } else {
        rope.line_to_char(start)
    };
    let end_line = (start + n_lines).min(total);
    let end_char = if end_line >= total {
        rope.len_chars()
    } else {
        rope.line_to_char(end_line)
    };
    rope.remove(start_char..end_char);
    rope.insert(start_char, text);
}

/// Count the number of ropey-style lines the snapshot occupies. A trailing
/// `\n` closes the final line; otherwise the dangling content counts as one
/// more line.
fn rope_line_count(s: &str) -> usize {
    if s.is_empty() {
        return 0;
    }
    let nls = s.bytes().filter(|b| *b == b'\n').count();
    if s.ends_with('\n') { nls } else { nls + 1 }
}
