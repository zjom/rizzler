//! Panel — the unified input/overlay container.
//!
//! A panel is any container that can receive keys: an editor window leaf, the
//! minibuffer when command-mode is active, or a popup overlay. They differ
//! only in *how* they get drawn, captured by [`PanelKind`]:
//!
//! - [`PanelKind::Minibuffer`] — drawn by the frame fn's `(w-minibuffer)`
//!   leaf at whatever rect it claims. No chrome.
//! - [`PanelKind::Overlay`] — a floating panel with explicit [`Placement`]
//!   and its own widget tree.
//!
//! Editor windows are *not* in the panel stack — they're a separate concept
//! (the [`crate::WindowTree`] still owns split layout + per-leaf BufferId).
//! The "is this an editor window?" focus is implicit: it's where focus goes
//! when no panel is on the stack.
//!
//! The [`PanelStack`] is the single source of truth for "where do keys go."
//! Top of stack wins; bottom of stack falls through to the focused editor
//! window leaf.

use std::rc::Rc;

use ratatui::layout::Rect;
use rizz::runtime::{RuntimeError, Value};
use rizz_text::{Buffer, BufferId, WrapConfig, WrapMap, WrapMode};

use crate::widget::Widget;

#[derive(Clone, Copy, Debug)]
pub enum Dim {
    Cells(u16),
    Frac(f32),
    /// Resolve to the minimum size required to contain the panel's content
    /// at its configured wrap mode. The actual fit value is computed
    /// per-frame by [`resolve_overlay_rect`] from the backing buffer.
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
    /// Cursor-anchored: resolved at render time against the focused window
    /// leaf's area and the focused buffer's cursor screen position.
    /// Placed one row below the cursor when there's room, otherwise above.
    /// Width hugs `width` (typically `Fit`). When no anchor is available
    /// (no focused leaf), falls back to `Centered`.
    AtCursor {
        width: Dim,
        height: Dim,
    },
    Full,
}

/// Captures where the focused buffer's cursor lives in frame coordinates,
/// plus the bounds of the focused window leaf. Threaded into the popup
/// placement pass by the renderer so `Placement::AtCursor` can resolve.
#[derive(Clone, Copy, Debug)]
pub struct CursorAnchor {
    /// Frame-relative bounds of the focused window leaf — popups stay inside
    /// this when possible.
    pub leaf: Rect,
    /// Cursor's absolute frame coords.
    pub cursor_x: u16,
    pub cursor_y: u16,
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
            // `AtCursor` needs a `CursorAnchor` — the plain `resolve` path
            // has no access to one, so fall back to the editor area's
            // centered default. The render path uses `resolve_with_anchor`
            // which handles `AtCursor` properly.
            Placement::AtCursor { width, height } => Placement::Centered { width, height }
                .resolve(area, fit_w, fit_h),
            Placement::Full => area,
        }
    }

    /// Resolution path for popups: same as [`Self::resolve`] for every
    /// variant except [`Placement::AtCursor`], which uses `anchor` (if
    /// available) to place itself adjacent to the cursor. Falls back to
    /// `Centered` when no anchor is available.
    pub fn resolve_with_anchor(
        &self,
        area: Rect,
        fit_w: u16,
        fit_h: u16,
        anchor: Option<CursorAnchor>,
    ) -> Rect {
        match *self {
            Placement::AtCursor { width, height } => {
                let Some(a) = anchor else {
                    return Placement::Centered { width, height }
                        .resolve(area, fit_w, fit_h);
                };
                let leaf = a.leaf;
                let h = height
                    .resolve(leaf.height, fit_h)
                    .max(1)
                    .min(leaf.height);
                let w = width
                    .resolve(leaf.width, fit_w)
                    .max(1)
                    .min(leaf.width);
                // Prefer below the cursor; fall back to above when there's
                // more room there. Below-room counts the rows under the
                // cursor row (exclusive of the cursor's own row); above-room
                // counts the rows above it.
                let below_room = leaf.bottom().saturating_sub(a.cursor_y + 1);
                let above_room = a.cursor_y.saturating_sub(leaf.y);
                let y = if below_room >= h {
                    a.cursor_y.saturating_add(1)
                } else if above_room >= h {
                    a.cursor_y.saturating_sub(h)
                } else if below_room >= above_room {
                    // Neither side fully fits — pick whichever has more
                    // room and let the height clamp.
                    a.cursor_y.saturating_add(1)
                } else {
                    leaf.y
                };
                // Horizontal: anchor at cursor_x; shift left if the popup
                // would overflow the leaf's right edge.
                let max_x = leaf.right().saturating_sub(w);
                let x = a.cursor_x.min(max_x).max(leaf.x);
                // Clamp height to whatever room is actually available at the
                // chosen y.
                let avail_h = leaf.bottom().saturating_sub(y);
                let h = h.min(avail_h).max(1);
                Rect::new(x, y, w, h)
            }
            _ => self.resolve(area, fit_w, fit_h),
        }
    }
}

/// Parse a [`Placement`] from a lisp value. Accepts the shorthand idents
/// `'centered` / `'full`, or a map shape `{kind: ... w: ... h: ...}`. Used
/// by both `(popup-show ...)` and `(w-overlay ...)`.
pub fn parse_placement(v: &Rc<Value>) -> Result<Placement, RuntimeError> {
    match &**v {
        Value::Ident(s) | Value::Str(s) => match s.as_ref() {
            "center" | "centered" => Ok(Placement::default()),
            "full" => Ok(Placement::Full),
            other => Err(unknown_variant("placement", other)),
        },
        Value::Map(m) => {
            let kind = m
                .get(&strkey("kind"))
                .map(|k| as_ident_or_str(k, "placement.kind"))
                .transpose()?
                .map(|s| s.to_string())
                .unwrap_or_else(|| "center".to_string());
            match kind.as_str() {
                "center" | "centered" => {
                    let width = m
                        .get(&strkey("w"))
                        .or_else(|| m.get(&strkey("width")))
                        .map(parse_dim)
                        .transpose()?
                        .unwrap_or(Dim::Frac(0.6));
                    let height = m
                        .get(&strkey("h"))
                        .or_else(|| m.get(&strkey("height")))
                        .map(parse_dim)
                        .transpose()?
                        .unwrap_or(Dim::Frac(0.6));
                    Ok(Placement::Centered { width, height })
                }
                "at" => {
                    let x = m
                        .get(&strkey("x"))
                        .map(|v| as_int(v, "placement.x"))
                        .transpose()?
                        .unwrap_or(0)
                        .max(0) as u16;
                    let y = m
                        .get(&strkey("y"))
                        .map(|v| as_int(v, "placement.y"))
                        .transpose()?
                        .unwrap_or(0)
                        .max(0) as u16;
                    let width = m
                        .get(&strkey("w"))
                        .or_else(|| m.get(&strkey("width")))
                        .map(parse_dim)
                        .transpose()?
                        .unwrap_or(Dim::Cells(40));
                    let height = m
                        .get(&strkey("h"))
                        .or_else(|| m.get(&strkey("height")))
                        .map(parse_dim)
                        .transpose()?
                        .unwrap_or(Dim::Cells(10));
                    Ok(Placement::At {
                        x,
                        y,
                        width,
                        height,
                    })
                }
                "side" => {
                    let side = m.get(&strkey("side")).ok_or_else(|| {
                        RuntimeError::type_mismatch("placement.side", "ident|str", v)
                    })?;
                    let side = parse_side(side)?;
                    let size = m
                        .get(&strkey("size"))
                        .map(parse_dim)
                        .transpose()?
                        .unwrap_or(Dim::Fit);
                    Ok(Placement::Anchored { side, size })
                }
                "at-cursor" => {
                    let width = m
                        .get(&strkey("w"))
                        .or_else(|| m.get(&strkey("width")))
                        .map(parse_dim)
                        .transpose()?
                        .unwrap_or(Dim::Fit);
                    let height = m
                        .get(&strkey("h"))
                        .or_else(|| m.get(&strkey("height")))
                        .map(parse_dim)
                        .transpose()?
                        .unwrap_or(Dim::Fit);
                    Ok(Placement::AtCursor { width, height })
                }
                "full" => Ok(Placement::Full),
                other => Err(unknown_variant("placement.kind", other)),
            }
        }
        _ => Err(RuntimeError::type_mismatch("placement", "ident|str|map", v)),
    }
}

pub fn parse_dim(v: &Rc<Value>) -> Result<Dim, RuntimeError> {
    match &**v {
        Value::Int(n) => Ok(Dim::Cells((*n).max(0) as u16)),
        Value::Float(f) => Ok(Dim::Frac(f.into_inner() as f32)),
        Value::Ident(s) | Value::Str(s) if s.as_ref() == "fit" => Ok(Dim::Fit),
        _ => Err(RuntimeError::type_mismatch("dim", "int|float|'fit", v)),
    }
}

pub fn parse_side(v: &Rc<Value>) -> Result<Side, RuntimeError> {
    let s = as_ident_or_str(v, "side")?;
    match s.as_ref() {
        "top" => Ok(Side::Top),
        "bottom" => Ok(Side::Bottom),
        "left" => Ok(Side::Left),
        "right" => Ok(Side::Right),
        other => Err(unknown_variant("side", other)),
    }
}

fn as_int(v: &Rc<Value>, name: &str) -> Result<i64, RuntimeError> {
    v.as_int()
        .ok_or_else(|| RuntimeError::type_mismatch(name, "int", v))
}

fn as_ident_or_str(v: &Rc<Value>, name: &str) -> Result<Rc<str>, RuntimeError> {
    match &**v {
        Value::Ident(s) | Value::Str(s) => Ok(s.clone()),
        _ => Err(RuntimeError::type_mismatch(name, "ident|str", v)),
    }
}

fn unknown_variant(name: &str, got: &str) -> RuntimeError {
    RuntimeError::TypeMismatch {
        name: name.into(),
        expected: "known symbol".into(),
        got: got.into(),
    }
}

fn strkey(s: &str) -> Rc<Value> {
    Rc::new(Value::Str(s.into()))
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

/// One entry on the editor's input/overlay stack.
#[derive(Clone, Debug)]
pub struct Panel {
    /// Backing buffer for this panel. Keys routed to this panel mutate this
    /// buffer; the precompute pass treats it like any other buffer.
    pub buf: BufferId,
    /// Keymap mode layers active while this panel has focus, most-recent
    /// first. The keymap resolver consults `panel.keymap_layers + [buf.mode]`
    /// when this panel is on top.
    pub keymap_layers: Vec<Rc<str>>,
    /// Widget tree drawn inside this panel's outer rect. `None` for a
    /// minibuffer panel — that one's rect comes from the frame fn's
    /// `(w-minibuffer)` leaf and renders without chrome.
    pub widget: Option<Widget>,
    /// What sort of panel this is — drives how the renderer places it.
    pub kind: PanelKind,
}

#[derive(Clone, Debug)]
pub enum PanelKind {
    /// Pinned to the rect claimed by the frame fn's `(w-minibuffer)` leaf.
    /// No chrome; the only way it differs from an editor window leaf is that
    /// it's never inside the [`crate::WindowTree`] split layout.
    Minibuffer,
    /// A floating overlay drawn at `placement`. The accompanying widget
    /// (held on the [`Panel`] itself) describes its chrome + content,
    /// typically `(w-block (w-popup-self))`.
    ///
    /// `name` is the symbol the lisp `(popup-show NAME …)` builtin assigned
    /// to this popup. Names are unique within the panel stack: re-issuing
    /// `popup-show` with the same name updates and re-raises the existing
    /// panel instead of stacking a new one.
    Overlay {
        placement: Placement,
        show_cursor: bool,
        name: Rc<str>,
    },
}

impl Panel {
    pub fn minibuffer(buf: BufferId) -> Self {
        Self {
            buf,
            keymap_layers: Vec::new(),
            widget: None,
            kind: PanelKind::Minibuffer,
        }
    }

    pub fn is_overlay(&self) -> bool {
        matches!(self.kind, PanelKind::Overlay { .. })
    }

    pub fn is_minibuffer(&self) -> bool {
        matches!(self.kind, PanelKind::Minibuffer)
    }

    /// Get the overlay-specific fields. Returns `None` for non-overlay
    /// panels.
    pub fn as_overlay(&self) -> Option<(&Placement, &Widget, bool)> {
        match &self.kind {
            PanelKind::Overlay {
                placement,
                show_cursor,
                ..
            } => self.widget.as_ref().map(|w| (placement, w, *show_cursor)),
            _ => None,
        }
    }

    /// The name this overlay panel was registered under, or `None` for the
    /// minibuffer.
    pub fn overlay_name(&self) -> Option<&str> {
        match &self.kind {
            PanelKind::Overlay { name, .. } => Some(name),
            _ => None,
        }
    }
}

/// Walk an overlay panel's widget tree and return the rect where the
/// `(buffer-view)` leaf will be drawn, given the panel's outer placement
/// rect. Recognizes [`Widget::Block`] as a chrome wrapper that insets the
/// rect; for every other wrapper it descends through
/// [`Widget::children`], so new transparent wrappers don't have to be
/// registered here.
pub fn buffer_view_rect(widget: &Widget, outer: Rect, panel_buf: BufferId) -> Rect {
    match widget {
        Widget::BufferView { buf } if buf.unwrap_or(panel_buf) == panel_buf => outer,
        Widget::Block { border, child, .. } => {
            buffer_view_rect(child, inset_rect(outer, border.inset()), panel_buf)
        }
        _ => widget
            .children()
            .next()
            .map(|c| buffer_view_rect(c, outer, panel_buf))
            .unwrap_or(outer),
    }
}

/// Total `(horizontal, vertical)` cells the panel's chrome adds between the
/// outer placement rect and the `(buffer-view)` leaf. Used by
/// [`resolve_overlay_rect`] to translate content-fit dims into outer dims
/// without knowing the outer size first (insets don't depend on the rect).
pub fn buffer_view_inset(widget: &Widget, panel_buf: BufferId) -> (u16, u16) {
    match widget {
        Widget::BufferView { buf } if buf.unwrap_or(panel_buf) == panel_buf => (0, 0),
        Widget::Block { border, child, .. } => {
            let i = border.inset();
            let (cw, ch) = buffer_view_inset(child, panel_buf);
            (cw + 2 * i, ch + 2 * i)
        }
        _ => widget
            .children()
            .next()
            .map(|c| buffer_view_inset(c, panel_buf))
            .unwrap_or((0, 0)),
    }
}

fn inset_rect(outer: Rect, i: u16) -> Rect {
    Rect {
        x: outer.x + i,
        y: outer.y + i,
        width: outer.width.saturating_sub(2 * i),
        height: outer.height.saturating_sub(2 * i),
    }
}

/// Resolve an overlay panel's outer rect within `area`, honouring [`Dim::Fit`]
/// by computing the minimum rows/cols needed to contain `buf`'s text under
/// its configured wrap mode. Called per-frame so size tracks text edits and
/// terminal resizes.
///
/// Panics if `panel` is not an [`PanelKind::Overlay`] — only overlay panels
/// have a placement to resolve.
pub fn resolve_overlay_rect(
    panel: &Panel,
    area: Rect,
    buf: &Buffer,
    anchor: Option<CursorAnchor>,
) -> Rect {
    let (placement, widget, _) = panel
        .as_overlay()
        .expect("resolve_overlay_rect: panel must be an Overlay");
    if !placement_needs_fit(placement) {
        return placement.resolve_with_anchor(area, 0, 0, anchor);
    }
    let (inset_w, inset_h) = buffer_view_inset(widget, panel.buf);
    // Width budget for wrapping when fitting height: full available area
    // minus chrome. `wrap_column` (if set) overrides — that's the buffer's
    // explicit wrap target, narrower than the panel might end up.
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
    placement.resolve_with_anchor(area, fit_w, fit_h, anchor)
}

fn placement_needs_fit(p: &Placement) -> bool {
    fn is_fit(d: Dim) -> bool {
        matches!(d, Dim::Fit)
    }
    match *p {
        Placement::Centered { width, height }
        | Placement::At { width, height, .. }
        | Placement::AtCursor { width, height } => is_fit(width) || is_fit(height),
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

/// Stack of panels above the editor window tree. Bottom-to-top order: the
/// renderer paints overlays in slice order so the last entry ends up on top,
/// and the keymap resolver picks the topmost panel for input routing.
///
/// The minibuffer enters the stack only when command mode is active (push on
/// `set-mode 'command`, pop on `command-cancel`/`command-submit`). When the
/// stack is empty, focus falls through to the editor window leaf.
#[derive(Default)]
pub struct PanelStack {
    stack: Vec<Panel>,
}

impl PanelStack {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, panel: Panel) {
        self.stack.push(panel);
    }

    pub fn pop(&mut self) -> Option<Panel> {
        self.stack.pop()
    }

    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Panel> {
        self.stack.iter()
    }

    pub fn as_slice(&self) -> &[Panel] {
        &self.stack
    }

    pub fn top(&self) -> Option<&Panel> {
        self.stack.last()
    }

    pub fn top_buf(&self) -> Option<BufferId> {
        self.stack.last().map(|p| p.buf)
    }

    pub fn top_keymap_layer(&self) -> Option<Rc<str>> {
        self.stack
            .last()
            .and_then(|p| p.keymap_layers.last().cloned())
    }

    /// Keymap layers from the focused panel, most-recent first. Empty when
    /// no panel is on the stack (editor windows have no layered modes of
    /// their own — they just use the buffer's `EditingMode`).
    pub fn top_keymap_layers(&self) -> &[Rc<str>] {
        self.stack
            .last()
            .map(|p| p.keymap_layers.as_slice())
            .unwrap_or(&[])
    }

    /// True if the topmost panel is the minibuffer.
    pub fn minibuffer_focused(&self) -> bool {
        self.stack.last().is_some_and(|p| p.is_minibuffer())
    }

    /// Topmost overlay panel, if any. Skips a minibuffer panel that may be
    /// sitting on top of overlays. Used by the renderer's "overlay cursor"
    /// path and by `popup-close`/`popup-mode` lisp builtins.
    pub fn top_overlay(&self) -> Option<&Panel> {
        self.stack.iter().rev().find(|p| p.is_overlay())
    }

    /// Pop the topmost overlay (skipping a minibuffer if present). Returns
    /// the popped panel, or `None` if there's no overlay on the stack.
    pub fn pop_top_overlay(&mut self) -> Option<Panel> {
        let idx = self.stack.iter().rposition(|p| p.is_overlay())?;
        Some(self.stack.remove(idx))
    }

    /// Remove the overlay panel registered under `name` (regardless of
    /// stack position). Used by `popup-show` to re-raise an existing popup
    /// and by `popup-hide NAME` to dismiss a specific one.
    pub fn remove_overlay_by_name(&mut self, name: &str) -> Option<Panel> {
        let idx = self
            .stack
            .iter()
            .position(|p| p.overlay_name() == Some(name))?;
        Some(self.stack.remove(idx))
    }

    /// Pop the topmost minibuffer panel (skipping overlays). Used when
    /// exiting command mode while overlays may still be open.
    pub fn pop_minibuffer(&mut self) -> Option<Panel> {
        let idx = self.stack.iter().rposition(|p| p.is_minibuffer())?;
        Some(self.stack.remove(idx))
    }

    /// Iterate just the overlay panels, bottom-to-top.
    pub fn overlays(&self) -> impl Iterator<Item = &Panel> {
        self.stack.iter().filter(|p| p.is_overlay())
    }

    pub fn any_overlay(&self) -> bool {
        self.stack.iter().any(|p| p.is_overlay())
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

    /// Cursor near the top of a tall leaf — there's plenty of room below,
    /// so the popup drops one row beneath the cursor.
    #[test]
    fn at_cursor_drops_below_when_room() {
        let p = Placement::AtCursor {
            width: Dim::Cells(20),
            height: Dim::Cells(8),
        };
        let anchor = CursorAnchor {
            leaf: Rect::new(0, 0, 80, 24),
            cursor_x: 5,
            cursor_y: 3,
        };
        let r = p.resolve_with_anchor(Rect::new(0, 0, 80, 24), 20, 8, Some(anchor));
        assert_eq!(r.x, 5);
        assert_eq!(r.y, 4);
        assert_eq!(r.width, 20);
        assert_eq!(r.height, 8);
    }

    /// Cursor near the bottom — not enough room below, falls back to above.
    #[test]
    fn at_cursor_flips_above_when_no_room_below() {
        let p = Placement::AtCursor {
            width: Dim::Cells(20),
            height: Dim::Cells(8),
        };
        let anchor = CursorAnchor {
            leaf: Rect::new(0, 0, 80, 24),
            cursor_x: 5,
            cursor_y: 20,
        };
        let r = p.resolve_with_anchor(Rect::new(0, 0, 80, 24), 20, 8, Some(anchor));
        assert_eq!(r.x, 5);
        assert_eq!(r.y, 12); // 20 - 8
        assert_eq!(r.height, 8);
    }

    /// Cursor near the right edge — popup shifts left to stay inside leaf.
    #[test]
    fn at_cursor_shifts_left_to_fit_horizontally() {
        let p = Placement::AtCursor {
            width: Dim::Cells(20),
            height: Dim::Cells(4),
        };
        let anchor = CursorAnchor {
            leaf: Rect::new(0, 0, 30, 24),
            cursor_x: 25,
            cursor_y: 2,
        };
        let r = p.resolve_with_anchor(Rect::new(0, 0, 30, 24), 20, 4, Some(anchor));
        // leaf right edge = 30, w=20 → max x = 10. Cursor_x 25 clamped to 10.
        assert_eq!(r.x, 10);
        assert_eq!(r.width, 20);
    }

    /// Multi-window: the popup respects the focused leaf's bounds, not the
    /// frame bounds. Right-half leaf with cursor near its right edge → popup
    /// shifts to keep it inside the leaf, not the frame.
    #[test]
    fn at_cursor_respects_focused_leaf_in_split() {
        let p = Placement::AtCursor {
            width: Dim::Cells(15),
            height: Dim::Cells(4),
        };
        let anchor = CursorAnchor {
            leaf: Rect::new(40, 0, 40, 24), // right half of an 80-col frame
            cursor_x: 75,
            cursor_y: 5,
        };
        let r = p.resolve_with_anchor(Rect::new(0, 0, 80, 24), 15, 4, Some(anchor));
        // leaf right edge = 80, w=15 → max_x = 65. Cursor at 75 → x clamped to 65.
        assert_eq!(r.x, 65);
        assert!(r.x >= 40, "popup must not drift left of the leaf");
        assert_eq!(r.y, 6);
    }

    /// No anchor available → falls back to centered.
    #[test]
    fn at_cursor_falls_back_to_centered_without_anchor() {
        let p = Placement::AtCursor {
            width: Dim::Cells(20),
            height: Dim::Cells(8),
        };
        let r = p.resolve_with_anchor(Rect::new(0, 0, 80, 24), 20, 8, None);
        // Centered: 80-20 / 2 = 30, 24-8 / 2 = 8
        assert_eq!(r.x, 30);
        assert_eq!(r.y, 8);
    }
}
