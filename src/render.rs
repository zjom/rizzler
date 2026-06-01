use std::io;

use crate::{buffer::Buffer, keymap::KeyEvent, window::WindowTree};

/// Read-only view of the editor passed to renderers. Decoupling this from
/// `State` means a renderer can be implemented without depending on the
/// editor's internal type, and `State` can hand out a snapshot without
/// exposing its private fields.
pub struct StateSnapshot<'a> {
    /// All buffers, indexed by the window tree's leaves.
    pub bufs: &'a [Buffer],
    /// Window tree — the renderer walks it to lay out editor windows.
    pub windows: &'a WindowTree,
    /// The minibuffer — always present, may be empty.
    pub minibuffer: &'a Buffer,
    /// Whether key input is currently routed to the minibuffer.
    pub focus_minibuffer: bool,
    /// Index of the focused editor buffer in `bufs` — what the status line
    /// shows as the "current" buffer.
    pub bufno: usize,
    pub keyevent: Option<KeyEvent>,
    pub cursor_style: CursorStyle,
}

impl StateSnapshot<'_> {
    /// The buffer that currently receives keystrokes — used for things like
    /// the mode glyph in the status line.
    pub fn focused(&self) -> &Buffer {
        if self.focus_minibuffer {
            self.minibuffer
        } else {
            &self.bufs[self.windows.focused_bufno()]
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStyle {
    Bar,
    Block,
}

pub trait Renderer {
    fn render(&mut self, snap: StateSnapshot<'_>) -> io::Result<()>;
}
