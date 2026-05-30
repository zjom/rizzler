use std::io;

use crate::{buffer::Buffer, keymap::KeyEvent, mode::EditingMode};

/// Read-only view of the editor passed to renderers. Decoupling this from
/// `State` means a renderer can be implemented without depending on the
/// editor's internal type, and `State` can hand out a snapshot without
/// exposing its private fields.
pub struct StateSnapshot<'a> {
    pub buffer: &'a Buffer,
    pub mode: EditingMode,
    pub command_buf: &'a str,
    pub bufno: usize,
    pub keyevent: Option<KeyEvent>,
    pub cursor_style: CursorStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStyle {
    Bar,
    Block,
}

pub trait Renderer {
    fn render(&mut self, snap: StateSnapshot<'_>) -> io::Result<()>;
}
