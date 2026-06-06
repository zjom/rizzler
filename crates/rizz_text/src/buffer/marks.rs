//! Selection anchor + keymap mode-layer stack. The selection model is
//! deliberately thin: the anchor is set when entering a visual mode and
//! cleared on the way out; per-mode slicing lives in `rizz_core::selection`.

use std::rc::Rc;

use rizz_core::{EditingMode, Position, selection};

use super::Buffer;

impl Buffer {
    /// Push `name` to the top of the keymap mode stack. Idempotent: if
    /// already present, the existing entry is removed first so the layer
    /// ends up at the top.
    pub fn push_mode_layer(&mut self, name: Rc<str>) {
        self.mode_layers.retain(|m| m.as_ref() != name.as_ref());
        self.mode_layers.push(name);
    }

    /// Remove `name` from the mode stack. No-op when absent.
    pub fn remove_mode_layer(&mut self, name: &str) {
        self.mode_layers.retain(|m| m.as_ref() != name);
    }

    pub fn mode_layers(&self) -> &[Rc<str>] {
        &self.mode_layers
    }

    /// Active modes for keymap resolution, most-specific first. Stacked
    /// layers (most recent first) precede the buffer's base editing mode.
    pub fn active_modes(&self) -> Vec<Rc<str>> {
        let mut v: Vec<Rc<str>> = self.mode_layers.iter().rev().cloned().collect();
        v.push(self.mode.as_str().into());
        v
    }

    pub fn set_mode(&mut self, mode: EditingMode) {
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
