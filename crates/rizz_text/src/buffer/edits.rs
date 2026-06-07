//! Buffer-mutating edits — text insertion, deletion, and undo/redo. Each
//! mutator records a [`rizz_changetree::Delta`] as a splice (a char range
//! removed, a char range inserted) so undo/redo restore both the rope and
//! the cursor position. Consecutive `insert_char` calls coalesce into a
//! single delta — see [`Buffer::insert_batch_end`].

use std::rc::Rc;

use rizz_changetree::Delta;
use rizz_core::{EditingMode, Position};

use super::Buffer;

impl Buffer {
    pub fn insert_char(&mut self, c: char) {
        let cidx = self.cur_line_start() + self.file_pos.col + self.cursor_pos.col as usize;
        let abs_before = self.abs_pos();

        self.buf.insert_char(cidx, c);
        self.invalidate_wrap_cache();

        if c == '\n' {
            self.cursor_pos.row = self.cursor_pos.row.saturating_add(1);
            self.cursor_pos.col = 0;
        } else {
            self.cursor_pos.col = self.cursor_pos.col.saturating_add(1);
        }
        let abs_after = self.abs_pos();

        if self.insert_batch_end == Some(cidx) {
            let extended = self.changetree.extend_current(|d| {
                let mut s = String::with_capacity(d.inserted.len() + c.len_utf8());
                s.push_str(&d.inserted);
                s.push(c);
                d.inserted = Rc::from(s);
                d.cursor_after = (abs_after.row, abs_after.col);
            });
            if extended {
                self.insert_batch_end = Some(cidx + 1);
                return;
            }
        }

        self.changetree.track_change(Delta {
            at: cidx,
            removed: Rc::from(""),
            inserted: Rc::from(String::from(c)),
            cursor_before: (abs_before.row, abs_before.col),
            cursor_after: (abs_after.row, abs_after.col),
        });
        self.insert_batch_end = Some(cidx + 1);
    }

    pub fn delete_char(&mut self) {
        self.close_insert_batch();
        let cidx = self.cur_line_start() + self.file_pos.col + self.cursor_pos.col as usize;
        if cidx == 0 {
            return;
        }
        let removed_ch = match self.buf.get_char(cidx - 1) {
            Some(c) => c,
            None => return,
        };
        let abs_before = self.abs_pos();

        if removed_ch == '\n' {
            self.cursor_pos.row = self.cursor_pos.row.saturating_sub(1);
            self.cursor_pos.col = self.cur_line().len_chars().saturating_sub(1) as u16;
        } else {
            self.cursor_pos.col = self.cursor_pos.col.saturating_sub(1);
        }

        _ = self.buf.try_remove(cidx - 1..cidx);
        self.invalidate_wrap_cache();
        let abs_after = self.abs_pos();

        self.changetree.track_change(Delta {
            at: cidx - 1,
            removed: Rc::from(String::from(removed_ch)),
            inserted: Rc::from(""),
            cursor_before: (abs_before.row, abs_before.col),
            cursor_after: (abs_after.row, abs_after.col),
        });
    }

    /// Remove the char range `[start..end)` from the rope and track the
    /// edit as a single undo delta. Cursor lands at the start of the
    /// range (clamped by the current mode). Returns `true` if anything
    /// was removed. Pure deletion — no mode handling.
    pub fn delete_range(&mut self, start: usize, end: usize) -> bool {
        self.close_insert_batch();
        let len = self.buf.len_chars();
        let s = start.min(len);
        let e = end.min(len);
        if s >= e {
            return false;
        }

        let removed = self.buf.slice(s..e).to_string();
        let abs_before = self.abs_pos();
        let start_line = self.buf.char_to_line(s);
        let target_col = s - self.buf.line_to_char(start_line);

        self.buf.remove(s..e);
        self.invalidate_wrap_cache();

        self.land_cursor_at(start_line, target_col);
        let abs_after = self.abs_pos();

        self.changetree.track_change(Delta {
            at: s,
            removed: Rc::from(removed),
            inserted: Rc::from(""),
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
                        any |= self.delete_range(line_start + actual_lo, line_start + actual_hi);
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
        self.close_insert_batch();
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
        let removed_ch = self.buf.char(cidx);
        let abs_before = self.abs_pos();
        _ = self.buf.try_remove(cidx..cidx + 1);
        self.invalidate_wrap_cache();
        self.clamp_cursor();
        let abs_after = self.abs_pos();
        self.changetree.track_change(Delta {
            at: cidx,
            removed: Rc::from(String::from(removed_ch)),
            inserted: Rc::from(""),
            cursor_before: (abs_before.row, abs_before.col),
            cursor_after: (abs_after.row, abs_after.col),
        });
    }

    /// Reverse the most recent tracked edit and return whether anything
    /// happened. Cursor lands where it was just before that edit.
    pub fn undo(&mut self) -> bool {
        self.close_insert_batch();
        let Some(delta) = self.changetree.undo() else {
            return false;
        };
        let inserted_len = delta.inserted.chars().count();
        self.buf.remove(delta.at..delta.at + inserted_len);
        self.buf.insert(delta.at, &delta.removed);
        self.invalidate_wrap_cache();
        let (row, col) = delta.cursor_before;
        self.land_cursor_at(row, col);
        true
    }

    /// Reapply the most recently undone edit, if any. Cursor lands where it
    /// ended up the first time around.
    pub fn redo(&mut self) -> bool {
        self.close_insert_batch();
        let Some(delta) = self.changetree.redo() else {
            return false;
        };
        let removed_len = delta.removed.chars().count();
        self.buf.remove(delta.at..delta.at + removed_len);
        self.buf.insert(delta.at, &delta.inserted);
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
