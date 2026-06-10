//! Buffer-mutating edits — text insertion, deletion, and undo/redo. Each
//! mutator records a [`rizz_changetree::Delta`] as a splice (a char range
//! removed, a char range inserted) so undo/redo restore both the rope and
//! the cursor position. Consecutive `insert_char` calls coalesce into a
//! single delta — see [`Buffer::insert_batch_end`].

use std::rc::Rc;

use rizz_changetree::Delta;
use rizz_core::{EditingMode, Position};

use super::{Buffer, MoveKind};

/// In-flight speculative insertion. The chars have already been written to
/// the rope and the cursor has been advanced, but the changetree has *not*
/// been touched — `commit_speculation` promotes the run to a single tracked
/// delta, `rollback_speculation` unwinds the rope and restores the cursor.
#[derive(Debug, Clone)]
pub struct Speculation {
    /// Rope char index where the first speculative char was written.
    pub(crate) start_cidx: usize,
    /// Cursor absolute position immediately before speculation began.
    pub(crate) cursor_before: Position<usize>,
    /// Speculative chars in insertion order. Length equals the number of
    /// rope chars to unwind on rollback / track on commit.
    pub(crate) inserted: String,
}

/// In-flight Replace-mode session — buffers up overwrites + extensions so
/// vim's `<bs>` restoration semantics work, then lands the whole thing as
/// one tracked delta on commit.
///
/// `history[i]` describes what happened at the i-th forward step starting
/// at `start_cidx`: `Some(orig)` means a char `orig` was overwritten there,
/// `None` means the cursor was past the original line end and a fresh char
/// was inserted. `<bs>` pops the last entry and either restores `orig` or
/// deletes the just-inserted char, keeping the rope and history in sync.
///
/// All None entries (if any) come after all Some entries within a single
/// session, because the cursor only advances past EOL by typing — so any
/// non-overwrite mutation (cursor move, `<enter>`, mode change) ends the
/// session by flushing the batch.
#[derive(Debug, Clone)]
pub struct ReplaceBatch {
    pub(crate) start_cidx: usize,
    pub(crate) cursor_before: Position<usize>,
    pub(crate) history: Vec<Option<char>>,
}

impl Buffer {
    pub fn insert_char(&mut self, c: char) {
        let cidx = self.cur_line_start() + self.file_pos.col + self.cursor_pos.col as usize;
        let abs_before = self.abs_pos();

        self.goal_col = None;
        self.buf.insert_char(cidx, c);
        let mut buf = [0u8; 4];
        self.record_text_edit(cidx, "", c.encode_utf8(&mut buf));
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

    /// Insert `s` at the cursor as a single tracked edit. Unlike repeated
    /// [`Buffer::insert_char`] calls — which coalesce into an insert batch
    /// only while the cursor stays put — this records one delta up front, so
    /// the whole string undoes/redoes as a single step regardless of newlines.
    pub fn insert_many(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.close_insert_batch();
        let cidx = self.cur_line_start() + self.file_pos.col + self.cursor_pos.col as usize;
        let abs_before = self.abs_pos();

        self.buf.insert(cidx, s);
        self.record_text_edit(cidx, "", s);
        self.invalidate_wrap_cache();

        let newlines = s.chars().filter(|&c| c == '\n').count();
        let (new_row, new_col) = if newlines > 0 {
            let tail = s.chars().rev().take_while(|&c| c != '\n').count();
            (abs_before.row + newlines, tail)
        } else {
            (abs_before.row, abs_before.col + s.chars().count())
        };
        self.land_cursor_at(new_row, new_col);
        let abs_after = self.abs_pos();

        self.changetree.track_change(Delta {
            at: cidx,
            removed: Rc::from(""),
            inserted: Rc::from(s),
            cursor_before: (abs_before.row, abs_before.col),
            cursor_after: (abs_after.row, abs_after.col),
        });
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
        let mut buf = [0u8; 4];
        self.record_text_edit(cidx - 1, removed_ch.encode_utf8(&mut buf), "");
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

    /// Insert `c` speculatively: write to the rope and advance the cursor
    /// the same way [`Buffer::insert_char`] does, but defer the changetree
    /// entry. The caller (the keymap, on chord descent) commits or rolls
    /// back the staged run via [`Buffer::commit_speculation`] /
    /// [`Buffer::rollback_speculation`] once the chord resolves.
    pub fn insert_speculative_char(&mut self, c: char) {
        if self.speculation.is_none() {
            // A tracked insert batch can't coalesce across a speculation
            // boundary — close it so the eventual commit / rollback can
            // own the batch state cleanly.
            self.close_insert_batch();
            let cidx = self.cur_line_start() + self.file_pos.col + self.cursor_pos.col as usize;
            self.speculation = Some(Speculation {
                start_cidx: cidx,
                cursor_before: self.abs_pos(),
                inserted: String::new(),
            });
        }
        let cidx = self.cur_line_start() + self.file_pos.col + self.cursor_pos.col as usize;
        self.goal_col = None;
        self.buf.insert_char(cidx, c);
        let mut buf = [0u8; 4];
        self.record_text_edit(cidx, "", c.encode_utf8(&mut buf));
        self.invalidate_wrap_cache();
        if c == '\n' {
            self.cursor_pos.row = self.cursor_pos.row.saturating_add(1);
            self.cursor_pos.col = 0;
        } else {
            self.cursor_pos.col = self.cursor_pos.col.saturating_add(1);
        }
        if let Some(spec) = self.speculation.as_mut() {
            spec.inserted.push(c);
        }
    }

    /// Promote the in-flight speculation into a single tracked delta and
    /// open a fresh insert batch at its tail, so a subsequent `insert_char`
    /// at the cursor extends the same undo step (e.g. the user pressed
    /// `j` then `x` with a `jk` chord — the `jx` ends up as one delta).
    /// No-op when no speculation is active.
    pub fn commit_speculation(&mut self) {
        let Some(spec) = self.speculation.take() else {
            return;
        };
        if spec.inserted.is_empty() {
            return;
        }
        let nchars = spec.inserted.chars().count();
        let end_cidx = spec.start_cidx + nchars;
        let abs_after = self.abs_pos();
        self.changetree.track_change(Delta {
            at: spec.start_cidx,
            removed: Rc::from(""),
            inserted: Rc::from(spec.inserted),
            cursor_before: (spec.cursor_before.row, spec.cursor_before.col),
            cursor_after: (abs_after.row, abs_after.col),
        });
        self.insert_batch_end = Some(end_cidx);
    }

    /// Unwind the in-flight speculation: remove the staged chars from the
    /// rope and restore the cursor to its pre-speculation position. Nothing
    /// is recorded in the changetree, so undo history is unaffected.
    /// No-op when no speculation is active.
    pub fn rollback_speculation(&mut self) {
        let Some(spec) = self.speculation.take() else {
            return;
        };
        if spec.inserted.is_empty() {
            return;
        }
        let nchars = spec.inserted.chars().count();
        let end_cidx = spec.start_cidx + nchars;
        _ = self.buf.try_remove(spec.start_cidx..end_cidx);
        self.record_text_edit(spec.start_cidx, &spec.inserted, "");
        self.invalidate_wrap_cache();
        self.land_cursor_at(spec.cursor_before.row, spec.cursor_before.col);
        self.insert_batch_end = None;
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
        self.record_text_edit(s, &removed, "");
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

    /// Delete `count` whole lines starting at the cursor (vim `dd` / `Ndd`).
    /// Eats the preceding newline when the deletion runs to EOF so the file
    /// doesn't gain a dangling trailing blank line. Returns whether anything
    /// changed.
    pub fn delete_line(&mut self, count: u32) -> bool {
        self.close_insert_batch();
        let n = count.max(1) as usize;
        let start_row = self.abs_row();
        let last_line = self.buf.len_lines().saturating_sub(1);
        if start_row > last_line {
            return false;
        }
        let end_row = (start_row + n - 1).min(last_line);
        self.delete_line_range(start_row, end_row)
    }

    /// Delete from the cursor to `kind`'s target (vim `d<motion>`). Vertical
    /// or whole-file motions delete entire lines (`dj`, `dk`, `dgg`, `dG`,
    /// `dLineNum`); everything else deletes the spanned character range.
    /// Returns whether anything changed.
    pub fn delete_motion(&mut self, kind: MoveKind, count: u32) -> bool {
        self.close_insert_batch();

        let start_pos = self.abs_pos();
        let start_cidx = self.buf.line_to_char(start_pos.row) + start_pos.col;

        // Run the motion under Insert-mode clamping so it can land past the
        // last char of a line — needed for `dl` at end-of-line, `d$`, etc.
        let saved_cursor = self.cursor_pos;
        let saved_file = self.file_pos;
        let saved_mode = self.mode;
        self.mode = EditingMode::Insert;
        self.move_cursor_n(kind, count);
        let end_pos = self.abs_pos();
        let end_cidx = self.buf.line_to_char(end_pos.row) + end_pos.col;
        self.cursor_pos = saved_cursor;
        self.file_pos = saved_file;
        self.mode = saved_mode;

        if is_linewise_motion(kind) {
            let lo = start_pos.row.min(end_pos.row);
            let hi = start_pos.row.max(end_pos.row);
            return self.delete_line_range(lo, hi);
        }

        let len = self.buf.len_chars();
        let (s, e) = if start_cidx <= end_cidx {
            let end = if is_inclusive_motion(kind) {
                (end_cidx + 1).min(len)
            } else {
                end_cidx
            };
            (start_cidx, end)
        } else {
            (end_cidx, start_cidx)
        };

        self.delete_range(s, e)
    }

    /// Delete every line in `[lo_row..=hi_row]`, joining the surrounding
    /// content so no blank line is left behind. Used by `dd` and by `dj`/`dk`
    /// linewise motions.
    fn delete_line_range(&mut self, lo_row: usize, hi_row: usize) -> bool {
        let total = self.buf.len_lines();
        if total == 0 {
            return false;
        }
        let last_line = total.saturating_sub(1);
        let hi_row = hi_row.min(last_line);
        let lo_row = lo_row.min(last_line);

        let (s, e) = if hi_row >= last_line && lo_row > 0 {
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

    /// Shift lines `lo_row..=hi_row` by one shift width — right when
    /// `dedent` is false (vim `>>`), left when true (vim `<<`) — recorded
    /// as a single tracked edit. Indenting prepends a shift width of spaces
    /// to every non-blank line and leaves blank lines untouched; dedenting
    /// removes up to one shift width of leading whitespace per line (a tab
    /// counts as a full shift width). The cursor lands on the first
    /// non-blank char of `lo_row`. Returns whether anything changed.
    pub fn shift_lines(&mut self, lo_row: usize, hi_row: usize, dedent: bool) -> bool {
        self.close_insert_batch();
        let total = self.buf.len_lines();
        if total == 0 {
            return false;
        }
        let last_line = total.saturating_sub(1);
        let lo_row = lo_row.min(last_line);
        let hi_row = hi_row.min(last_line).max(lo_row);

        let s = self.buf.line_to_char(lo_row);
        let e = if hi_row + 1 >= total {
            self.buf.len_chars()
        } else {
            self.buf.line_to_char(hi_row + 1)
        };

        let removed = self.buf.slice(s..e).to_string();
        let mut inserted = String::with_capacity(removed.len() + SHIFT_WIDTH);
        let mut changed = false;
        for line in removed.split_inclusive('\n') {
            let (body, nl) = match line.strip_suffix('\n') {
                Some(b) => (b, "\n"),
                None => (line, ""),
            };
            if dedent {
                let drop = leading_dedent_bytes(body);
                changed |= drop > 0;
                inserted.push_str(&body[drop..]);
            } else if body.chars().all(char::is_whitespace) {
                // Vim leaves blank lines untouched on `>>`.
                inserted.push_str(body);
            } else {
                changed = true;
                inserted.extend(std::iter::repeat_n(' ', SHIFT_WIDTH));
                inserted.push_str(body);
            }
            inserted.push_str(nl);
        }

        if !changed {
            return false;
        }

        let abs_before = self.abs_pos();
        self.buf.remove(s..e);
        self.buf.insert(s, &inserted);
        self.record_text_edit(s, &removed, &inserted);
        self.invalidate_wrap_cache();

        // Land on the first shifted line's first non-blank column (vim).
        let col = inserted
            .split('\n')
            .next()
            .unwrap_or("")
            .chars()
            .take_while(|c| c.is_whitespace())
            .count();
        self.land_cursor_at(lo_row, col);
        let abs_after = self.abs_pos();

        self.changetree.track_change(Delta {
            at: s,
            removed: Rc::from(removed),
            inserted: Rc::from(inserted),
            cursor_before: (abs_before.row, abs_before.col),
            cursor_after: (abs_after.row, abs_after.col),
        });
        true
    }

    /// Vim `>>` / `<<` — shift `count` lines starting at the cursor row one
    /// shift width right (`dedent=false`) or left (`dedent=true`). Returns
    /// whether anything changed.
    pub fn shift_line(&mut self, count: u32, dedent: bool) -> bool {
        let n = count.max(1) as usize;
        let lo = self.abs_row();
        let last = self.buf.len_lines().saturating_sub(1);
        if lo > last {
            return false;
        }
        let hi = (lo + n - 1).min(last);
        self.shift_lines(lo, hi, dedent)
    }

    /// Vim `>` / `<` in a visual mode — shift the lines the selection spans
    /// and return to Normal mode. No-op when not in a visual mode.
    pub fn shift_selection(&mut self, dedent: bool) -> bool {
        let changed = match self.selection_anchor {
            Some(anchor) if self.mode.is_visual() => {
                let cursor = self.abs_pos();
                let lo = anchor.row.min(cursor.row);
                let hi = anchor.row.max(cursor.row);
                self.shift_lines(lo, hi, dedent)
            }
            _ => false,
        };
        self.set_mode(EditingMode::Normal);
        changed
    }

    /// Vim `r<char>` — replace up to `count` chars starting at the cursor
    /// with `c`, recorded as a single tracked edit. Stops at the trailing
    /// newline (so the line's length never changes) and cursor lands on the
    /// last replaced char. Returns whether anything changed.
    pub fn replace_char_n(&mut self, c: char, count: u32) -> bool {
        self.close_insert_batch();
        let n = count.max(1) as usize;
        let abs = self.abs_pos();
        let line = self.buf.line(abs.row);
        let line_len = line.len_chars();
        let has_trailing_nl = line_len > 0 && line.char(line_len - 1) == '\n';
        let usable = if has_trailing_nl {
            line_len - 1
        } else {
            line_len
        };
        if abs.col >= usable {
            return false;
        }
        let take = n.min(usable - abs.col);
        let line_start = self.buf.line_to_char(abs.row);
        let s = line_start + abs.col;
        let e = s + take;
        let removed = self.buf.slice(s..e).to_string();
        let inserted: String = std::iter::repeat(c).take(take).collect();
        let abs_before = abs;

        self.buf.remove(s..e);
        self.buf.insert(s, &inserted);
        self.record_text_edit(s, &removed, &inserted);
        self.invalidate_wrap_cache();

        // Land cursor on the last replaced char.
        self.land_cursor_at(abs.row, abs.col + take - 1);
        let abs_after = self.abs_pos();

        self.changetree.track_change(Delta {
            at: s,
            removed: Rc::from(removed),
            inserted: Rc::from(inserted),
            cursor_before: (abs_before.row, abs_before.col),
            cursor_after: (abs_after.row, abs_after.col),
        });
        true
    }

    /// Vim Replace-mode keystroke — overwrite the char under the cursor with
    /// `c` and advance by one. At end-of-line (or on the trailing newline)
    /// the char is inserted instead, extending the line. The rope is
    /// mutated immediately but the change is buffered into
    /// [`Buffer::replace_batch`]; the changetree only sees the final delta
    /// when the batch commits (mode change, cursor move, foreign edit).
    /// That batching is what makes vim's `<bs>` original-char restore
    /// possible and keeps the whole session as one undo step.
    pub fn overwrite_char(&mut self, c: char) {
        if self.replace_batch.is_none() {
            self.start_replace_batch();
        }
        let abs = self.abs_pos();
        let line = self.buf.line(abs.row);
        let line_len = line.len_chars();
        let has_trailing_nl = line_len > 0 && line.char(line_len - 1) == '\n';
        let usable = if has_trailing_nl {
            line_len - 1
        } else {
            line_len
        };
        let cidx = self.buf.line_to_char(abs.row) + abs.col;

        let mut new_buf = [0u8; 4];
        let new_str = c.encode_utf8(&mut new_buf);
        let entry = if abs.col >= usable {
            self.buf.insert_char(cidx, c);
            self.record_text_edit(cidx, "", new_str);
            None
        } else {
            let orig = self.buf.char(cidx);
            self.buf.remove(cidx..cidx + 1);
            self.buf.insert_char(cidx, c);
            let mut orig_buf = [0u8; 4];
            self.record_text_edit(cidx, orig.encode_utf8(&mut orig_buf), new_str);
            Some(orig)
        };
        self.invalidate_wrap_cache();
        if let Some(batch) = self.replace_batch.as_mut() {
            batch.history.push(entry);
        }
        self.land_cursor_at(abs.row, abs.col + 1);
    }

    /// Vim Replace-mode `<bs>` — walk back over the last `overwrite_char` and
    /// undo its effect on the rope: restore the original char if the slot
    /// was overwritten, delete the inserted char if it was an extension.
    /// No-op (and returns `false`) when the batch is empty — `<bs>` past the
    /// session's starting position is a beep in vim.
    pub fn replace_backspace(&mut self) -> bool {
        let Some(batch) = self.replace_batch.as_mut() else {
            return false;
        };
        let Some(entry) = batch.history.pop() else {
            return false;
        };
        let new_len = batch.history.len();
        let target_cidx = batch.start_cidx + new_len;
        let abs = self.abs_pos();
        let removed_ch = self.buf.char(target_cidx);
        self.buf.remove(target_cidx..target_cidx + 1);
        if let Some(orig) = entry {
            self.buf.insert_char(target_cidx, orig);
        }
        let mut removed_buf = [0u8; 4];
        let mut inserted_buf = [0u8; 4];
        let removed_str = removed_ch.encode_utf8(&mut removed_buf);
        let inserted_str = match entry {
            Some(orig) => orig.encode_utf8(&mut inserted_buf),
            None => "",
        };
        self.record_text_edit(target_cidx, removed_str, inserted_str);
        self.invalidate_wrap_cache();
        self.land_cursor_at(abs.row, abs.col.saturating_sub(1));
        true
    }

    /// Begin a Replace-mode session at the cursor. Called from `set_mode`
    /// on entry; also a safety net for `overwrite_char` if a caller managed
    /// to set Replace mode behind its back. `close_insert_batch` flushes
    /// any prior replace batch + insert batch as part of breaking them.
    pub(crate) fn start_replace_batch(&mut self) {
        self.close_insert_batch();
        let abs = self.abs_pos();
        let cidx = self.buf.line_to_char(abs.row) + abs.col;
        self.replace_batch = Some(ReplaceBatch {
            start_cidx: cidx,
            cursor_before: abs,
            history: Vec::new(),
        });
    }

    /// Flush the in-flight Replace-mode session to the changetree as one
    /// delta. No-op when no session is active, when the session typed
    /// nothing, or when overwrites + backspaces cancelled each other out
    /// (the rope is back to where it started). Called from `set_mode`,
    /// `move_cursor`, and the rest of the rope-mutating methods so the
    /// batch never outlives a state change that would invalidate its
    /// `start_cidx` / `history` invariants.
    pub(crate) fn commit_replace_batch(&mut self) {
        let Some(batch) = self.replace_batch.take() else {
            return;
        };
        let n = batch.history.len();
        if n == 0 {
            return;
        }
        let removed: String = batch.history.iter().filter_map(|x| *x).collect();
        let end = (batch.start_cidx + n).min(self.buf.len_chars());
        let inserted = self.buf.slice(batch.start_cidx..end).to_string();
        if removed == inserted {
            return;
        }
        let abs_after = self.abs_pos();
        self.changetree.track_change(Delta {
            at: batch.start_cidx,
            removed: Rc::from(removed),
            inserted: Rc::from(inserted),
            cursor_before: (batch.cursor_before.row, batch.cursor_before.col),
            cursor_after: (abs_after.row, abs_after.col),
        });
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
        let mut buf = [0u8; 4];
        self.record_text_edit(cidx, removed_ch.encode_utf8(&mut buf), "");
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

    /// Vim `g;` — jump to the position of the last edit. Repeated calls walk
    /// further back through the change tree (per-leaf parent chain). `count`
    /// takes `count` steps back in one go. Returns whether the cursor moved.
    pub fn goto_last_edit(&mut self, count: u32) -> bool {
        self.close_insert_batch();
        let Some((row, col)) = self.changetree.walk_back_edit(count) else {
            return false;
        };
        self.land_cursor_at(row, col);
        true
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
        // Undo's "removed" is what we just put back, "inserted" is what we
        // just removed — flipped relative to the original edit.
        self.record_text_edit(delta.at, &delta.inserted, &delta.removed);
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
        self.record_text_edit(delta.at, &delta.removed, &delta.inserted);
        self.invalidate_wrap_cache();
        let (row, col) = delta.cursor_after;
        self.land_cursor_at(row, col);
        true
    }

    /// Public re-export of [`Self::land_cursor_at`] for cross-crate callers
    /// (e.g. the LSP integration jumping to a goto-definition target).
    /// Coordinates are absolute (file_pos + cursor_pos); the implementation
    /// clamps row to the last line and column to the line's usable length.
    pub fn land_cursor_to(&mut self, row: usize, col: usize) {
        self.land_cursor_at(row, col);
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

/// Columns one `>>` / `<<` shifts a line by, and the count of spaces an
/// indent inserts.
const SHIFT_WIDTH: usize = 4;

/// Leading bytes to drop to dedent `line` by one shift width: each space is
/// one column, a tab a full shift width; stops once a shift width has been
/// consumed or a non-whitespace char is hit. Leading whitespace is ASCII,
/// so the byte count equals the char count.
fn leading_dedent_bytes(line: &str) -> usize {
    let mut cols = 0;
    let mut bytes = 0;
    for c in line.chars() {
        if cols >= SHIFT_WIDTH {
            break;
        }
        match c {
            ' ' => cols += 1,
            '\t' => cols += SHIFT_WIDTH,
            _ => break,
        }
        bytes += c.len_utf8();
    }
    bytes
}

/// Motions whose `d<motion>` form deletes whole lines instead of a char
/// range — vertical relatives, file ends, and explicit line jumps.
fn is_linewise_motion(kind: MoveKind) -> bool {
    use MoveKind as MK;
    match kind {
        MK::FileStart | MK::FileEnd | MK::LineNum(_) => true,
        MK::Relative(p) if p.row != 0 => true,
        _ => false,
    }
}

/// Motions where vim treats the target character as part of the operated
/// range — only the end-of-word family. Forward motions otherwise stop just
/// before the target, and backward motions naturally include the target by
/// virtue of the `[end, start)` slice.
fn is_inclusive_motion(kind: MoveKind) -> bool {
    matches!(kind, MoveKind::WordEnd | MoveKind::BigWordEnd)
}
