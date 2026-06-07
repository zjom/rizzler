//! Popup overlay primitive.
//!
//! A popup is just a [`crate::Widget`] with extras: a [`Placement`] that
//! re-resolves against the editor area each frame, a stack of keymap modes
//! that capture input while the popup is on top, and a backing buffer that
//! input routes to. All chrome (border + title + faces) lives in the widget
//! tree itself.

use std::rc::Rc;

use ratatui::layout::Rect;
use rizz_text::{Buffer, WrapConfig, WrapMap, WrapMode};

use crate::widget::Widget;

#[derive(Clone, Copy, Debug)]
pub enum Dim {
    Cells(u16),
    Frac(f32),
    /// Resolve to the minimum size required to contain the popup's content
    /// at the popup's configured wrap mode. The actual fit value is computed
    /// per-frame by [`resolve_popup_rect`] from the backing buffer.
    Fit,
}

impl Dim {
    /// `fit` is the precomputed "minimum to contain content" hint along this
    /// axis. Callers that don't know fit pass `0` — `Dim::Fit` will collapse
    /// to that, which the placement layer clamps to a minimum of 1.
    pub fn resolve(self, total: u16, fit: u16) -> u16 {
        match self {
            Dim::Cells(n) => n.min(total),
            Dim::Frac(f) => {
                let f = f.clamp(0.0, 1.0);
                ((total as f32) * f).round() as u16
            }
            Dim::Fit => fit.min(total),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Top,
    Bottom,
    Left,
    Right,
}

#[derive(Clone, Debug)]
pub enum Placement {
    Centered {
        width: Dim,
        height: Dim,
    },
    At {
        x: u16,
        y: u16,
        width: Dim,
        height: Dim,
    },
    Anchored {
        side: Side,
        size: Dim,
    },
    Full,
}

impl Default for Placement {
    fn default() -> Self {
        Self::Centered {
            width: Dim::Frac(0.6),
            height: Dim::Frac(0.6),
        }
    }
}

impl Placement {
    /// Resolve to a screen rect within `area`. `fit_w`/`fit_h` are the
    /// per-axis content-fit hints used by [`Dim::Fit`]; pass `0` when fit
    /// isn't applicable.
    pub fn resolve(&self, area: Rect, fit_w: u16, fit_h: u16) -> Rect {
        match *self {
            Placement::Centered { width, height } => {
                let w = width.resolve(area.width, fit_w).max(2).min(area.width);
                let h = height.resolve(area.height, fit_h).max(2).min(area.height);
                let x = area.x + area.width.saturating_sub(w) / 2;
                let y = area.y + area.height.saturating_sub(h) / 2;
                Rect::new(x, y, w, h)
            }
            Placement::At {
                x,
                y,
                width,
                height,
            } => {
                let x = x.min(area.width.saturating_sub(1));
                let y = y.min(area.height.saturating_sub(1));
                let w = width
                    .resolve(area.width, fit_w)
                    .min(area.width.saturating_sub(x))
                    .max(1);
                let h = height
                    .resolve(area.height, fit_h)
                    .min(area.height.saturating_sub(y))
                    .max(1);
                Rect::new(area.x + x, area.y + y, w, h)
            }
            Placement::Anchored { side, size } => match side {
                Side::Top => {
                    let h = size.resolve(area.height, fit_h).max(1).min(area.height);
                    Rect::new(area.x, area.y, area.width, h)
                }
                Side::Bottom => {
                    let h = size.resolve(area.height, fit_h).max(1).min(area.height);
                    Rect::new(
                        area.x,
                        area.y + area.height.saturating_sub(h),
                        area.width,
                        h,
                    )
                }
                Side::Left => {
                    let w = size.resolve(area.width, fit_w).max(1).min(area.width);
                    Rect::new(area.x, area.y, w, area.height)
                }
                Side::Right => {
                    let w = size.resolve(area.width, fit_w).max(1).min(area.width);
                    Rect::new(
                        area.x + area.width.saturating_sub(w),
                        area.y,
                        w,
                        area.height,
                    )
                }
            },
            Placement::Full => area,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BorderStyle {
    None,
    #[default]
    Plain,
    Rounded,
    Double,
    Thick,
}

impl BorderStyle {
    pub fn inset(self) -> u16 {
        match self {
            BorderStyle::None => 0,
            _ => 1,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Popup {
    pub bufno: usize,
    pub placement: Placement,
    /// The widget tree drawn at the resolved placement rect.
    pub widget: Widget,
    /// Keymap mode layers pushed onto the popup's buffer when it opens.
    pub mode_layers: Vec<Rc<str>>,
    /// Whether to display the popup buffer's cursor inside the popup.
    pub show_cursor: bool,
}

/// Walk a popup widget tree and return the rect where the
/// `(buffer-view)` leaf will be drawn, given the popup's outer placement
/// rect.
pub fn buffer_view_rect(widget: &Widget, outer: Rect, popup_bufno: usize) -> Rect {
    match widget {
        Widget::BufferView { bufno } if bufno.unwrap_or(popup_bufno) == popup_bufno => outer,
        Widget::Block { border, child, .. } => {
            let inset = border.inset();
            let inner = Rect {
                x: outer.x + inset,
                y: outer.y + inset,
                width: outer.width.saturating_sub(2 * inset),
                height: outer.height.saturating_sub(2 * inset),
            };
            buffer_view_rect(child, inner, popup_bufno)
        }
        Widget::Constrained { child, .. } => buffer_view_rect(child, outer, popup_bufno),
        _ => outer,
    }
}

/// Total `(horizontal, vertical)` cells the popup's chrome adds between the
/// outer placement rect and the `(buffer-view)` leaf. Used by
/// [`resolve_popup_rect`] to translate content-fit dims into outer dims
/// without knowing the outer size first (insets don't depend on the rect).
pub fn buffer_view_inset(widget: &Widget, popup_bufno: usize) -> (u16, u16) {
    match widget {
        Widget::BufferView { bufno } if bufno.unwrap_or(popup_bufno) == popup_bufno => (0, 0),
        Widget::Block { border, child, .. } => {
            let i = border.inset();
            let (cw, ch) = buffer_view_inset(child, popup_bufno);
            (cw + 2 * i, ch + 2 * i)
        }
        Widget::Constrained { child, .. } => buffer_view_inset(child, popup_bufno),
        _ => (0, 0),
    }
}

/// Resolve a popup's outer rect within `area`, honouring [`Dim::Fit`] by
/// computing the minimum rows/cols needed to contain `buf`'s text under its
/// configured wrap mode. Called per-frame so size tracks text edits and
/// terminal resizes.
pub fn resolve_popup_rect(popup: &Popup, area: Rect, buf: &Buffer) -> Rect {
    if !placement_needs_fit(&popup.placement) {
        return popup.placement.resolve(area, 0, 0);
    }
    let (inset_w, inset_h) = buffer_view_inset(&popup.widget, popup.bufno);
    // Width budget for wrapping when fitting height: full available area
    // minus chrome. `wrap_column` (if set) overrides — that's the buffer's
    // explicit wrap target, narrower than the popup might end up.
    let inner_w = match buf.wrap_column() {
        Some(c) => c,
        None => area.width.saturating_sub(inset_w),
    };
    let max_rows = area.height.saturating_sub(inset_h).max(1) as usize;
    let cfg = WrapConfig {
        mode: buf.wrap_mode(),
        width: inner_w,
        breakindent: buf.breakindent(),
    };
    let map = WrapMap::build(buf, 0, max_rows, cfg);
    let fit_inner_h = map.rows.len() as u16;
    // Longest line width — for wrap-mode none this is the unwrapped length;
    // for wrap-on it's the wrap target. Used for Centered/At/Left/Right fits.
    let fit_inner_w = match buf.wrap_mode() {
        WrapMode::None => longest_line_chars(buf, max_rows),
        _ => inner_w,
    };
    let fit_w = fit_inner_w.saturating_add(inset_w);
    let fit_h = fit_inner_h.saturating_add(inset_h);
    popup.placement.resolve(area, fit_w, fit_h)
}

fn placement_needs_fit(p: &Placement) -> bool {
    fn is_fit(d: Dim) -> bool {
        matches!(d, Dim::Fit)
    }
    match *p {
        Placement::Centered { width, height } | Placement::At { width, height, .. } => {
            is_fit(width) || is_fit(height)
        }
        Placement::Anchored { size, .. } => is_fit(size),
        Placement::Full => false,
    }
}

fn longest_line_chars(buf: &Buffer, max_rows: usize) -> u16 {
    let mut longest = 0u16;
    for (i, line) in buf.lines_at(0).enumerate() {
        if i >= max_rows {
            break;
        }
        let mut n = line.len_chars();
        if n > 0 && line.char(n - 1) == '\n' {
            n -= 1;
        }
        longest = longest.max(n.min(u16::MAX as usize) as u16);
    }
    longest
}

/// Overlay stack, bottom-to-top.
#[derive(Default)]
pub struct PopupStack {
    stack: Vec<Popup>,
}

impl PopupStack {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, popup: Popup) {
        self.stack.push(popup);
    }

    pub fn pop(&mut self) -> Option<Popup> {
        self.stack.pop()
    }

    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Popup> {
        self.stack.iter()
    }

    pub fn as_slice(&self) -> &[Popup] {
        &self.stack
    }

    pub fn top_mode(&self) -> Option<Rc<str>> {
        self.stack
            .last()
            .and_then(|p| p.mode_layers.last().cloned())
    }

    pub fn top_bufno(&self) -> Option<usize> {
        self.stack.last().map(|p| p.bufno)
    }

    /// Re-index `bufno` references after the buffer at `removed` was deleted
    /// from `State.bufs`. Indices past `removed` shift down by one.
    pub fn shift_bufnos_after_removal(&mut self, removed: usize) {
        for p in &mut self.stack {
            if p.bufno > removed {
                p.bufno -= 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centered_default_is_60_percent() {
        let p = Placement::default();
        let r = p.resolve(Rect::new(0, 0, 100, 50), 0, 0);
        assert_eq!(r.width, 60);
        assert_eq!(r.height, 30);
        assert_eq!(r.x, 20);
        assert_eq!(r.y, 10);
    }

    #[test]
    fn full_placement_fills_area() {
        let area = Rect::new(2, 3, 80, 24);
        let r = Placement::Full.resolve(area, 0, 0);
        assert_eq!(r, area);
    }

    #[test]
    fn anchored_bottom_sticks_to_floor() {
        let r = Placement::Anchored {
            side: Side::Bottom,
            size: Dim::Cells(5),
        }
        .resolve(Rect::new(0, 0, 80, 24), 0, 0);
        assert_eq!(r.y, 19);
        assert_eq!(r.height, 5);
        assert_eq!(r.width, 80);
    }

    #[test]
    fn fit_dim_resolves_to_hint_clamped_by_total() {
        assert_eq!(Dim::Fit.resolve(20, 5), 5);
        assert_eq!(Dim::Fit.resolve(20, 100), 20);
        assert_eq!(Dim::Fit.resolve(20, 0), 0);
    }

    #[test]
    fn anchored_bottom_with_fit_uses_height_hint() {
        let r = Placement::Anchored {
            side: Side::Bottom,
            size: Dim::Fit,
        }
        .resolve(Rect::new(0, 0, 80, 24), 0, 7);
        assert_eq!(r.height, 7);
        assert_eq!(r.y, 17);
        assert_eq!(r.width, 80);
    }
}
