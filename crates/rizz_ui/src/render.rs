//! Renderer trait, read-only state snapshot, and the precomputed per-frame
//! data the renderer consumes. Decoupling these from `State` lets a
//! renderer be written without depending on editor internals and lets
//! `State` hand out a snapshot without exposing its private fields.

use std::io;
use std::rc::Rc;

use ratatui::text::Line;

use rizz_core::Display;
use rizz_input::KeyEvent;
use rizz_text::{Buffer, BufferId, WrapMap};
use slotmap::{SecondaryMap, SlotMap};

use crate::{
    panel::PanelStack,
    styling::{Style, Theme},
    widget::Widget,
    window::WindowTree,
};

pub use rizz_core::Display as DisplayRe;

pub struct StateSnapshot<'a> {
    pub bufs: &'a SlotMap<BufferId, Buffer>,
    pub windows: &'a WindowTree,
    pub minibuffer: &'a Buffer,
    /// Id of the focused editor buffer.
    pub buf: BufferId,
    pub keyevent: Option<KeyEvent>,
    pub cursor_style: CursorStyle,
    /// Bottom-to-top stack of panels above the window tree; the topmost
    /// entry has focus.
    pub panels: &'a PanelStack,
}

impl StateSnapshot<'_> {
    /// Buffer that currently receives keystrokes.
    pub fn focused(&self) -> &Buffer {
        match self.panels.top_buf() {
            Some(id) => &self.bufs[id],
            None => &self.bufs[self.windows.focused_buf()],
        }
    }

    pub fn focus_minibuffer(&self) -> bool {
        self.panels.minibuffer_focused()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStyle {
    Bar,
    Block,
}

/// Output of the precompute pass — the renderer is a pure consumer of
/// this, no lisp call happens from inside `Renderer::render`.
pub struct RenderedFrame {
    /// Baseline fg/bg the renderer fills the whole frame with first.
    pub default_style: Style,
    pub theme: Theme,
    /// Frame layout with no callables and gutters already pre-rendered.
    pub root: Widget,
    /// Per-buffer precomputed data; only buffers visible this frame are
    /// populated. `Rc` because entries may be shared with (and reused from)
    /// the frame-to-frame [`crate::precompute::PrecomputeCache`].
    pub per_buf: SecondaryMap<BufferId, Rc<RenderedBuffer>>,
}

#[derive(Default)]
pub struct RenderedBuffer {
    pub gutter: Option<RenderedGutter>,
    pub decorators: Vec<DecoratorRanges>,
    /// Ranges-by-row index into `decorators`: entry `row - viewport_start`
    /// holds `(decorator_idx, range_idx)` pairs in paint order, so the
    /// renderer touches only the ranges on the row it's drawing instead of
    /// scanning every range of every decorator per row.
    pub row_index: Vec<Vec<(u32, u32)>>,
    /// File row of `row_index[0]`.
    pub viewport_start: usize,
    /// Visual-line layout for soft-wrapped buffers. `None` = no wrap.
    /// When `Some`, the first entry corresponds to `buf.file_pos().row`
    /// and the slice covers at least the viewport height.
    pub wrap: Option<WrapMap>,
}

impl RenderedBuffer {
    /// Paint-order ranges that touch file row `row`, resolved through
    /// `row_index`.
    pub fn ranges_on_row(&self, row: usize) -> impl Iterator<Item = &StyledRange> {
        row.checked_sub(self.viewport_start)
            .and_then(|i| self.row_index.get(i))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
            .iter()
            .map(|&(di, ri)| &self.decorators[di as usize].ranges[ri as usize])
    }
}

pub struct RenderedGutter {
    pub width: u16,
    /// One row per visible viewport row, already padded to `width`.
    pub rows: Vec<Line<'static>>,
}

/// `Fit` measures the widest row the gutter fn returns this frame.
/// `Fixed(n)` reserves exactly `n` cells. `Fixed(0)` disables the gutter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GutterWidth {
    #[default]
    Fit,
    Fixed(u16),
}

/// Styled character ranges applied after a buffer's base line content.
/// Indices are absolute (file row, byte column within the row).
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
    /// Pad the highlight to the area's full width regardless of how many
    /// real characters the line has.
    pub pad_to_width: bool,
    /// When set, the renderer *replaces* chars in `[col, col+len)` with
    /// this content instead of restyling them.
    pub display: Option<Display>,
}

pub trait Renderer {
    fn render(&mut self, snap: StateSnapshot<'_>, frame: &RenderedFrame) -> io::Result<()>;
}
