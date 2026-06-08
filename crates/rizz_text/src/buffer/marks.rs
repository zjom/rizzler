//! Selection anchor + buffer editing mode. The selection model is
//! deliberately thin: the anchor is set when entering a visual mode and
//! cleared on the way out; per-mode slicing lives in `rizz_core::selection`.
//!
//! Buffers carry only their primary [`EditingMode`]. The stack of named
//! keymap layers a popup or minibuffer panel layers on top lives on the
//! panel itself ([`rizz_ui::panel::Panel::keymap_layers`]) so that one
//! buffer can serve as the backing buffer of multiple panels without
//! their input contexts bleeding into each other.

use rizz_core::{EditingMode, Position, selection};

use super::Buffer;

impl Buffer {
    pub fn set_mode(&mut self, mode: EditingMode) {
        if self.mode != mode {
            self.close_insert_batch();
        }
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
        selection::selected_text(&self.buf, self.mode, anchor, self.abs_pos())
    }
}
