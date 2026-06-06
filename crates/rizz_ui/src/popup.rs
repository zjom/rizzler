//! Popup overlay primitive.
//!
//! A popup is just a [`crate::Widget`] with extras: a [`Placement`] that
//! re-resolves against the editor area each frame, a stack of keymap modes
//! that capture input while the popup is on top, and a backing buffer that
//! input routes to. All chrome (border + title + faces) lives in the widget
//! tree itself.

use std::rc::Rc;

use ratatui::layout::Rect;

use crate::widget::Widget;

#[derive(Clone, Copy, Debug)]
pub enum Dim {
    Cells(u16),
    Frac(f32),
}

impl Dim {
    pub fn resolve(self, total: u16) -> u16 {
        match self {
            Dim::Cells(n) => n.min(total),
            Dim::Frac(f) => {
                let f = f.clamp(0.0, 1.0);
                ((total as f32) * f).round() as u16
            }
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
    pub fn resolve(&self, area: Rect) -> Rect {
        match *self {
            Placement::Centered { width, height } => {
                let w = width.resolve(area.width).max(2).min(area.width);
                let h = height.resolve(area.height).max(2).min(area.height);
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
                    .resolve(area.width)
                    .min(area.width.saturating_sub(x))
                    .max(1);
                let h = height
                    .resolve(area.height)
                    .min(area.height.saturating_sub(y))
                    .max(1);
                Rect::new(area.x + x, area.y + y, w, h)
            }
            Placement::Anchored { side, size } => match side {
                Side::Top => {
                    let h = size.resolve(area.height).max(1).min(area.height);
                    Rect::new(area.x, area.y, area.width, h)
                }
                Side::Bottom => {
                    let h = size.resolve(area.height).max(1).min(area.height);
                    Rect::new(
                        area.x,
                        area.y + area.height.saturating_sub(h),
                        area.width,
                        h,
                    )
                }
                Side::Left => {
                    let w = size.resolve(area.width).max(1).min(area.width);
                    Rect::new(area.x, area.y, w, area.height)
                }
                Side::Right => {
                    let w = size.resolve(area.width).max(1).min(area.width);
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
        let r = p.resolve(Rect::new(0, 0, 100, 50));
        assert_eq!(r.width, 60);
        assert_eq!(r.height, 30);
        assert_eq!(r.x, 20);
        assert_eq!(r.y, 10);
    }

    #[test]
    fn full_placement_fills_area() {
        let area = Rect::new(2, 3, 80, 24);
        let r = Placement::Full.resolve(area);
        assert_eq!(r, area);
    }

    #[test]
    fn anchored_bottom_sticks_to_floor() {
        let r = Placement::Anchored {
            side: Side::Bottom,
            size: Dim::Cells(5),
        }
        .resolve(Rect::new(0, 0, 80, 24));
        assert_eq!(r.y, 19);
        assert_eq!(r.height, 5);
        assert_eq!(r.width, 80);
    }
}
