//! Selection anchor + buffer editing mode. The selection model is
//! deliberately thin: the anchor is set when entering a visual mode and
//! cleared on the way out; per-mode slicing lives in `rizz_core::selection`.
//!
//! Buffers carry only their primary [`EditingMode`]. The stack of named
//! keymap layers a popup or minibuffer panel layers on top lives on the
//! panel itself ([`rizz_ui::panel::Panel::keymap_layers`]) so that one
//! buffer can serve as the backing buffer of multiple panels without
//! their input contexts bleeding into each other.

use rizz_core::{EditingMode, FilePos, Position, selection};

use super::Buffer;

impl Buffer {
    pub fn set_mode(&mut self, mode: EditingMode) {
        if self.mode != mode {
            // Flushes any in-flight Replace-mode session too, since
            // `close_insert_batch` commits the replace batch on the way out.
            self.close_insert_batch();
        }
        let was_visual = self.mode.is_visual();
        let is_visual = mode.is_visual();
        if is_visual && !was_visual {
            self.selection_anchor = Some(self.abs_pos());
        } else if !is_visual {
            self.selection_anchor = None;
        }
        let was_replace = self.mode == EditingMode::Replace;
        self.mode = mode;
        if mode == EditingMode::Replace && !was_replace {
            self.start_replace_batch();
        }
    }

    /// Anchor of the current visual selection (absolute file position).
    pub fn selection_anchor(&self) -> Option<FilePos> {
        self.selection_anchor
    }

    /// Text covered by the current visual selection. Inclusive on both ends;
    /// `VisualLine` includes the trailing newline of the last selected row,
    /// and `VisualBlock` joins each row's column slice with `\n`.
    pub fn selected_text(&self) -> Option<String> {
        let anchor = self.selection_anchor?;
        selection::selected_text(&self.buf, self.mode, anchor, self.abs_pos())
    }

    /// Char count of [`Self::selected_text`] without building the string —
    /// for per-frame consumers like the status line.
    pub fn selection_size(&self) -> Option<usize> {
        let anchor = self.selection_anchor?;
        selection::selection_size(&self.buf, self.mode, anchor, self.abs_pos())
    }

    /// Set up a charwise visual selection spanning the half-open char range
    /// `[start, end)`. Switches the buffer into Visual mode, anchors at
    /// `start`, and lands the cursor on the last char of the range so the
    /// selection covers exactly the requested span. No-op when `end <= start`.
    pub fn select_char_range(&mut self, start: usize, end: usize) {
        if end <= start {
            return;
        }
        let total_chars = self.buf.len_chars();
        let end = end.min(total_chars);
        let start = start.min(end);
        let start_row = self.buf.char_to_line(start);
        let start_col = start - self.buf.line_to_char(start_row);
        let last = end - 1;
        let end_row = self.buf.char_to_line(last);
        let end_col = last - self.buf.line_to_char(end_row);

        self.set_mode(EditingMode::Visual);
        self.selection_anchor = Some(Position::new(start_col, start_row));
        self.land_cursor_at(end_row, end_col);
    }
}
