use std::io;

use ratatui::text::Line;

use crate::{
    buffer::Buffer,
    keymap::KeyEvent,
    popup::Popup,
    styling::{Style, Theme},
    widget::Widget,
    window::WindowTree,
    wrap::WrapMap,
};

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
    /// Popup stack, bottom-to-top. The renderer paints them in order, so the
    /// last entry ends up on top. The cursor of the focused editor window
    /// is hidden while this slice is non-empty; the topmost popup may opt
    /// into showing its own cursor via `Popup::show_cursor`.
    pub popups: &'a [Popup],
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

// ---------------------------------------------------------------------------
// RenderedFrame
// ---------------------------------------------------------------------------

/// Everything the renderer needs that came out of the precompute pass.
///
/// The precompute pass calls the user's `set-frame` fn to build a widget
/// tree, parses it into [`Widget`], pre-renders per-buffer gutters and
/// decorator ranges, and snapshots the theme. The renderer is then a pure
/// consumer — no lisp call happens from inside `Renderer::render`.
pub struct RenderedFrame {
    /// Resolved style of the `default` face — the baseline fg/bg the renderer
    /// fills the whole frame with before drawing anything else.
    pub default_style: Style,
    /// Snapshot of the theme as of the precompute pass. The renderer reads it
    /// to resolve face references on widgets and popup chrome.
    pub theme: Theme,
    /// The user-supplied frame layout. Already concrete — no callables remain
    /// in the tree, gutters have been pre-rendered into `per_buf`.
    pub root: Widget,
    /// Per-buffer precomputed data: gutter rows (built from the EditorTree's
    /// gutter fn), built-in + user decorator ranges, and the soft-wrap layout.
    pub per_buf: Vec<RenderedBuffer>,
}

/// Per-buffer precomputed data — gutter rows and decorator ranges.
#[derive(Default)]
pub struct RenderedBuffer {
    pub gutter: Option<RenderedGutter>,
    pub decorators: Vec<DecoratorRanges>,
    /// Visual-line layout for soft-wrapped buffers. `None` = no wrap;
    /// renderer falls back to one screen row per file row. When `Some`,
    /// the first entry corresponds to `buf.file_pos().row` and the slice
    /// covers at least the viewport height.
    pub wrap: Option<WrapMap>,
}

pub struct RenderedGutter {
    /// Fixed column width registered with the gutter.
    pub width: u16,
    /// One [`Line`] per visible row of the buffer's viewport. Already
    /// padded to `width`.
    pub rows: Vec<Line<'static>>,
}

/// Whole-buffer decorator output: a flat list of styled character ranges
/// to apply after the base line content is drawn. Indices are absolute
/// (file row, byte column within the row).
#[derive(Default)]
pub struct DecoratorRanges {
    pub ranges: Vec<StyledRange>,
}

#[derive(Clone, Debug)]
pub struct StyledRange {
    pub row: usize,
    pub col: usize,
    pub len: usize,
    pub style: Style,
    /// Pad the highlight out to the area's full width, regardless of how
    /// many real characters the line has on this row. Used by the
    /// current-line-highlight decorator so the band fills the screen.
    pub pad_to_width: bool,
    /// If set, the renderer *replaces* the underlying chars in `[col, col+len)`
    /// with this display content instead of restyling them. `style` applies
    /// to the displayed content. Used for fold ellipses, virtual text, and
    /// inline hints.
    pub display: Option<Display>,
}

/// Visual substitution for a range. `String` replaces with arbitrary text;
/// `Space(n)` replaces with `n` blank cells.
#[derive(Clone, Debug)]
pub enum Display {
    String(std::rc::Rc<str>),
    Space(usize),
}

pub trait Renderer {
    fn render(&mut self, snap: StateSnapshot<'_>, frame: &RenderedFrame) -> io::Result<()>;
}
