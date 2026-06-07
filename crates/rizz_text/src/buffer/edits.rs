//! Buffer-mutating edits — text insertion, deletion, and undo/redo. Each
//! mutator records a [`rizz_changetree::Delta`] so undo/redo restore both
//! the rope contents and the cursor position.

use rizz_changetree::Delta;
use rizz_core::{EditingMode, Position};
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

    /// Remove the char range `[start..end)` from the rope and track the
    /// edit as a single undo delta. Cursor lands at the start of the
    /// range (clamped by the current mode). Returns `true` if anything
    /// was removed. Pure deletion — no mode handling.
    pub fn delete_range(&mut self, start: usize, end: usize) -> bool {
        let len = self.buf.len_chars();
        let s = start.min(len);
        let e = end.min(len);
        if s >= e {
            return false;
        }

        let start_line = self.buf.char_to_line(s);
        let end_line = self.buf.char_to_line(e - 1);
        let before_n_lines = end_line - start_line + 1;
        let before = snapshot_lines(&self.buf, start_line, before_n_lines);
        let abs_before = self.abs_pos();

        // Did the range cover whole lines exactly? If so we removed N
        // lines outright; otherwise the partial lines collapse into one.
        let total = self.buf.len_lines();
        let s_at_line_start = s == self.buf.line_to_char(start_line);
        let next_line_char = if end_line + 1 >= total {
            len
        } else {
            self.buf.line_to_char(end_line + 1)
        };
        let e_at_line_end = e == next_line_char;
        let whole_lines = s_at_line_start && e_at_line_end;

        let target_col = s - self.buf.line_to_char(start_line);

        self.buf.remove(s..e);
        self.invalidate_wrap_cache();

        let after_n_lines = if whole_lines { 0 } else { 1 };
        let after = snapshot_lines(&self.buf, start_line, after_n_lines);

        self.land_cursor_at(start_line, target_col);
        let abs_after = self.abs_pos();

        self.changetree.track_change(Delta {
            start_line,
            before: before.into(),
            after: after.into(),
            cursor_before: (abs_before.row, abs_before.col),
            cursor_after: (abs_after.row, abs_after.col),
        });

        true
    }

    /// Delete the current visual selection and return to Normal mode.
    /// Convenience wrapper around [`Buffer::delete_range`]: computes the
    /// char range(s) implied by the buffer's mode + anchor + cursor, calls
    /// `delete_range`, and switches the mode. No-op when the buffer is
    /// not in a visual mode. For VisualBlock each row's column slice is
    /// a separate `delete_range` call (and therefore a separate undo step).
    pub fn delete_selection(&mut self) -> bool {
        let Some(anchor) = self.selection_anchor else {
            return false;
        };
        let mode = self.mode;
        if !mode.is_visual() {
            return false;
        }
        let cursor = self.abs_pos();

        let lo_row = anchor.row.min(cursor.row);
        let hi_row = anchor.row.max(cursor.row);
        let lo_col = anchor.col.min(cursor.col);
        let hi_col = anchor.col.max(cursor.col);

        let (lo_pos, hi_pos) = if (anchor.row, anchor.col) <= (cursor.row, cursor.col) {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        };

        let changed = match mode {
            EditingMode::Visual => {
                let s = self.buf.line_to_char(lo_pos.row) + lo_pos.col;
                let e = self.buf.line_to_char(hi_pos.row) + hi_pos.col + 1;
                self.delete_range(s, e)
            }
            EditingMode::VisualLine => {
                let total = self.buf.len_lines();
                let last_line = total.saturating_sub(1);
                let (s, e) = if hi_row >= last_line && lo_row > 0 {
                    // Eat the preceding newline so deleting through EOF
                    // doesn't leave a dangling trailing newline.
                    (self.buf.line_to_char(lo_row) - 1, self.buf.len_chars())
                } else {
                    let s = self.buf.line_to_char(lo_row);
                    let e = if hi_row + 1 >= total {
                        self.buf.len_chars()
                    } else {
                        self.buf.line_to_char(hi_row + 1)
                    };
                    (s, e)
                };
                self.delete_range(s, e)
            }
            EditingMode::VisualBlock => {
                // Iterate bottom-up so each row's char indices remain
                // valid as we mutate the rope.
                let mut any = false;
                for row in (lo_row..=hi_row).rev() {
                    let line_start = self.buf.line_to_char(row);
                    let line = self.buf.line(row);
                    let mut line_len = line.len_chars();
                    if line_len > 0 && line.char(line_len - 1) == '\n' {
                        line_len -= 1;
                    }
                    let actual_lo = lo_col.min(line_len);
                    let actual_hi = (hi_col + 1).min(line_len);
                    if actual_lo < actual_hi {
                        any |= self
                            .delete_range(line_start + actual_lo, line_start + actual_hi);
                    }
                }
                if any {
                    self.land_cursor_at(lo_row, lo_col);
                }
                any
            }
            _ => false,
        };

        self.set_mode(EditingMode::Normal);
        changed
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
