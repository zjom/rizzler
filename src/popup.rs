//! Popup overlay primitive.
//!
//! A [`Popup`] is conceptually a buffer drawn on top of the editor area,
//! with chrome (border + title), a placement that re-resolves against the
//! editor area each frame, and a keymap mode that captures input while the
//! popup is on top of the stack.
//!
//! Popups deliberately reuse the rest of the editor's machinery:
//!
//! * Content comes from a regular [`crate::buffer::Buffer`] of kind
//!   [`crate::buffer::BufferKind::Popup`] — text properties, overlays, the
//!   cursor, and editing modes all behave as they would in a window.
//! * Styling references go through the shared [`crate::styling::Theme`] —
//!   the `face` / `border_face` / `title_face` fields are face names and
//!   resolve via the same `face-define` / `face-of` machinery used by
//!   status segments and decorators.
//! * Key bindings live in the same [`crate::keymap::KeymapRegistry`] under a
//!   user-chosen mode (default `"popup"`). Custom popups can use names like
//!   `"popup.files"` and bind keys to them in lisp.
//!
//! That is what lets a "popup terminal" or "popup file explorer" be a popup
//! with a different `keymap_mode` and a producer filling its buffer — no
//! popup-specific code path beyond placement and chrome.

use std::rc::Rc;

use ratatui::layout::Rect;

/// A length measurement that can be expressed either as a cell count or as a
/// fraction of the available editor area. `Frac` is clamped to `[0.0, 1.0]`
/// at resolve time so accidental out-of-range values can't break layout.
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

/// Where a popup sits within the editor area.
#[derive(Clone, Debug)]
pub enum Placement {
    /// Centered on `area`. The legacy message popup uses `Frac(0.6)` for
    /// both axes — that's the default constructor.
    Centered { width: Dim, height: Dim },
    /// Fixed top-left corner + size, relative to `area.x` / `area.y`.
    At {
        x: u16,
        y: u16,
        width: Dim,
        height: Dim,
    },
    /// Pinned to one side of the editor; extent along the perpendicular axis
    /// always fills the editor.
    Anchored { side: Side, size: Dim },
    /// Cover the editor area entirely.
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
    /// Concrete rectangle within `area`. The result is always inside `area`
    /// (clamped both for size and for position).
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

/// Border line set the renderer draws around the popup. Mapped to ratatui's
/// `BorderType` at render time so the popup module doesn't pull ratatui's
/// widget surface into its public API.
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
    /// `1` if the border reserves a cell on each side, `0` if it doesn't.
    /// Used by `State::refresh_viewport` to size the popup buffer's
    /// viewport against the inner content area.
    pub fn inset(self) -> u16 {
        match self {
            BorderStyle::None => 0,
            _ => 1,
        }
    }
}

/// Visual decoration around the popup's content. Every face field references
/// a name from the shared [`crate::styling::Theme`] — popups deliberately
/// reuse the same face machinery as status segments, decorators, and gutters.
#[derive(Clone, Debug, Default)]
pub struct Chrome {
    pub border: BorderStyle,
    pub title: Option<Rc<str>>,
    /// Face used to fill the popup's interior background. `None` falls back
    /// to the editor's `default` face.
    pub face: Option<Rc<str>>,
    /// Face for the border characters.
    pub border_face: Option<Rc<str>>,
    /// Face for the title text drawn into the top border.
    pub title_face: Option<Rc<str>>,
}

/// A popup overlay. `bufno` always points at a popup-kind buffer in
/// `State.bufs` for the duration of the popup's life.
#[derive(Clone, Debug)]
pub struct Popup {
    pub bufno: usize,
    pub placement: Placement,
    pub chrome: Chrome,
    /// Keymap mode used while this popup is on top of the stack. Defaults to
    /// `"popup"`. Custom popups can use names like `"popup.files"` and bind
    /// keys to them in lisp.
    pub keymap_mode: Rc<str>,
    /// Whether to display the popup buffer's cursor inside the popup.
    /// Off by default — viewing popups (messages, hints) don't want a
    /// cursor; editable popups (terminal, prompt) flip this on.
    pub show_cursor: bool,
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

    #[test]
    fn at_clamps_to_area() {
        let r = Placement::At {
            x: 100,
            y: 100,
            width: Dim::Cells(20),
            height: Dim::Cells(5),
        }
        .resolve(Rect::new(0, 0, 80, 24));
        assert!(r.width >= 1 && r.x < 80);
        assert!(r.height >= 1 && r.y < 24);
    }

    #[test]
    fn border_inset_reflects_style() {
        assert_eq!(BorderStyle::None.inset(), 0);
        assert_eq!(BorderStyle::Plain.inset(), 1);
        assert_eq!(BorderStyle::Rounded.inset(), 1);
    }
}
