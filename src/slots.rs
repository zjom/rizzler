//! Registry of customization slots — status segments, gutters, line decorators,
//! and bottom-strip components. Each slot is identified by an ordered position
//! plus a name (for add/remove/replace operations). The renderer's precompute
//! pass (in `state.rs`, see commit 3) walks this registry once per frame.
//!
//! Three kinds of slot payload:
//!
//! * [`LispRenderable::Static`] — a literal lisp value, consumed verbatim.
//! * [`LispRenderable::Callable`] — a closure or native fn called each frame
//!   with kind-specific arguments.
//! * [`LispRenderable::Builtin`] — a built-in Rust producer identified by
//!   [`BuiltinId`]. This is how the bundled `default-style.lisp` reuses the
//!   existing Rust gutters/segments/decorators without going through the
//!   lisp callback path.

use std::rc::Rc;

use ratatui::text::{Line, Span};
use rizz::RizzError;
use rizz::runtime::{self, Env, Value};

use crate::buffer::Buffer;
use crate::mode::EditingMode;
use crate::render::{DecoratorRanges, RenderedGutter, StateSnapshot, StyledRange};
use crate::styling::{Style, Theme, rgb_value, spans_from_value, style_from_value};

// ---------------------------------------------------------------------------
// Renderable payload
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum LispRenderable {
    Static(Rc<Value>),
    Callable(Rc<Value>),
    Builtin(BuiltinId),
}

/// Identifiers for the Rust-side producers that lisp can reference by ident.
///
/// These let `default-style.lisp` write `(decorator-add 'current-line-highlight)`
/// and reuse the existing optimized Rust impl, rather than recreating the
/// behavior in lisp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinId {
    // gutters
    LineNumbers,
    // line decorators
    BaseFg,
    SelectionHighlight,
    CurrentLineHighlight,
    // status segments
    ModeGlyph,
    LastKey,
    BufferNo,
}

impl BuiltinId {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "line-numbers" => Self::LineNumbers,
            "base-fg" => Self::BaseFg,
            "selection-highlight" => Self::SelectionHighlight,
            "current-line-highlight" => Self::CurrentLineHighlight,
            "mode-glyph" => Self::ModeGlyph,
            "last-key" => Self::LastKey,
            "buffer-no" => Self::BufferNo,
            _ => return None,
        })
    }

    /// The slot category this builtin belongs to. Used to reject mismatches
    /// like `(status-segment-add 'line-numbers ...)`.
    pub fn category(self) -> SlotCategory {
        match self {
            Self::LineNumbers => SlotCategory::Gutter,
            Self::BaseFg | Self::SelectionHighlight | Self::CurrentLineHighlight => {
                SlotCategory::Decorator
            }
            Self::ModeGlyph | Self::LastKey | Self::BufferNo => SlotCategory::StatusSegment,
        }
    }
}

// ---------------------------------------------------------------------------
// Slot kinds
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotCategory {
    StatusSegment,
    Gutter,
    Decorator,
    Bottom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentSide {
    Left,
    Right,
}

/// Per-kind layout metadata threaded along with each slot. The category enum
/// above is the type-only discriminant; this carries the extra data needed
/// to lay out the slot (which side, how wide, how many rows).
#[derive(Clone, Debug)]
pub enum SlotKind {
    StatusSegment { side: SegmentSide },
    Gutter { width: u16 },
    Decorator,
    Bottom { rows: u16 },
}

impl SlotKind {
    pub fn category(&self) -> SlotCategory {
        match self {
            Self::StatusSegment { .. } => SlotCategory::StatusSegment,
            Self::Gutter { .. } => SlotCategory::Gutter,
            Self::Decorator => SlotCategory::Decorator,
            Self::Bottom { .. } => SlotCategory::Bottom,
        }
    }
}

// ---------------------------------------------------------------------------
// Slot + registry
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Slot {
    pub name: Rc<str>,
    pub kind: SlotKind,
    pub renderable: LispRenderable,
}

/// Ordered lists of slots per category. Mutated via `add`/`remove`/`replace`/
/// `clear`; iteration order within each list is the render order.
#[derive(Clone, Debug, Default)]
pub struct SlotRegistry {
    status_left: Vec<Slot>,
    status_right: Vec<Slot>,
    gutters: Vec<Slot>,
    decorators: Vec<Slot>,
    bottom: Vec<Slot>,
}

impl SlotRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn status_segments(&self, side: SegmentSide) -> &[Slot] {
        match side {
            SegmentSide::Left => &self.status_left,
            SegmentSide::Right => &self.status_right,
        }
    }

    pub fn gutters(&self) -> &[Slot] {
        &self.gutters
    }

    pub fn decorators(&self) -> &[Slot] {
        &self.decorators
    }

    pub fn bottom(&self) -> &[Slot] {
        &self.bottom
    }

    /// Append a slot to the appropriate list. If a slot with the same name
    /// already exists in that list, it is replaced in place (preserving its
    /// position) so re-registering is idempotent.
    pub fn add(&mut self, slot: Slot) {
        let list = self.list_mut(&slot.kind);
        match list.iter().position(|s| s.name == slot.name) {
            Some(i) => list[i] = slot,
            None => list.push(slot),
        }
    }

    /// Remove the slot named `name` from `category`. Returns `true` if a
    /// matching slot was found.
    pub fn remove(&mut self, category: SlotCategory, name: &str) -> bool {
        let list = match category {
            SlotCategory::StatusSegment => {
                // Try left first, then right.
                if remove_named(&mut self.status_left, name) {
                    return true;
                }
                &mut self.status_right
            }
            SlotCategory::Gutter => &mut self.gutters,
            SlotCategory::Decorator => &mut self.decorators,
            SlotCategory::Bottom => &mut self.bottom,
        };
        remove_named(list, name)
    }

    /// Replace an entire ordered list (status segments per side, gutters,
    /// decorators, bottom rows) with `new_slots` in source order.
    pub fn replace(
        &mut self,
        category: SlotCategory,
        side: Option<SegmentSide>,
        new_slots: Vec<Slot>,
    ) {
        let list = match (category, side) {
            (SlotCategory::StatusSegment, Some(SegmentSide::Left)) => &mut self.status_left,
            (SlotCategory::StatusSegment, Some(SegmentSide::Right)) => &mut self.status_right,
            (SlotCategory::Gutter, _) => &mut self.gutters,
            (SlotCategory::Decorator, _) => &mut self.decorators,
            (SlotCategory::Bottom, _) => &mut self.bottom,
            // Replacing status segments without specifying a side: clear both.
            (SlotCategory::StatusSegment, None) => {
                self.status_left.clear();
                self.status_right.clear();
                return;
            }
        };
        *list = new_slots;
    }

    pub fn clear(&mut self, category: SlotCategory) {
        match category {
            SlotCategory::StatusSegment => {
                self.status_left.clear();
                self.status_right.clear();
            }
            SlotCategory::Gutter => self.gutters.clear(),
            SlotCategory::Decorator => self.decorators.clear(),
            SlotCategory::Bottom => self.bottom.clear(),
        }
    }

    fn list_mut(&mut self, kind: &SlotKind) -> &mut Vec<Slot> {
        match kind {
            SlotKind::StatusSegment {
                side: SegmentSide::Left,
            } => &mut self.status_left,
            SlotKind::StatusSegment {
                side: SegmentSide::Right,
            } => &mut self.status_right,
            SlotKind::Gutter { .. } => &mut self.gutters,
            SlotKind::Decorator => &mut self.decorators,
            SlotKind::Bottom { .. } => &mut self.bottom,
        }
    }
}

fn remove_named(list: &mut Vec<Slot>, name: &str) -> bool {
    if let Some(i) = list.iter().position(|s| s.name.as_ref() == name) {
        list.remove(i);
        true
    } else {
        false
    }
}

// ---------------------------------------------------------------------------
// Producers
// ---------------------------------------------------------------------------
//
// One function per slot category. Each one dispatches on the slot's
// `renderable` (Static, Callable, Builtin) and returns the concrete data the
// renderer needs. Lisp errors are reported via `Result`; the precompute pass
// in `state.rs` is responsible for surfacing them.

/// Status segment producer. Returns a sequence of ratatui spans.
pub fn produce_status_segment(
    slot: &Slot,
    snap: &StateSnapshot<'_>,
    theme: &Theme,
    env: &Env,
) -> Result<Vec<Span<'static>>, RizzError> {
    match &slot.renderable {
        LispRenderable::Static(v) => Ok(spans_from_value(v, theme)?),
        LispRenderable::Callable(f) => {
            let v = runtime::apply(f, &[], env)?;
            Ok(spans_from_value(&v, theme)?)
        }
        LispRenderable::Builtin(b) => Ok(builtin_status_segment(*b, snap, theme)),
    }
}

/// Gutter producer. Returns one column's worth of pre-rendered rows for
/// `buf`, padded to a stable width.
pub fn produce_gutter(
    slot: &Slot,
    buf: &Buffer,
    theme: &Theme,
    env: &Env,
) -> Result<RenderedGutter, RizzError> {
    let SlotKind::Gutter {
        width: registered_width,
    } = slot.kind
    else {
        return Ok(RenderedGutter {
            width: 0,
            rows: vec![],
        });
    };

    match &slot.renderable {
        LispRenderable::Builtin(b) => Ok(builtin_gutter(*b, buf, theme)),
        LispRenderable::Static(_) | LispRenderable::Callable(_) => {
            let width = registered_width;
            let start = buf.file_pos().row.min(buf.len_lines());
            let visible = buf.viewport.row as usize;
            let last = buf.len_lines().saturating_sub(1);

            let mut rows = Vec::with_capacity(visible);
            for r in 0..visible {
                let lnum = start + r;
                let lnum_arg = if lnum <= last {
                    Rc::new(Value::Int(lnum as i64))
                } else {
                    Rc::new(Value::Unit)
                };
                let v = match &slot.renderable {
                    LispRenderable::Static(v) => v.clone(),
                    LispRenderable::Callable(f) => runtime::apply(f, &[lnum_arg], env)?,
                    LispRenderable::Builtin(_) => unreachable!(),
                };
                let spans = spans_from_value(&v, theme)?;
                rows.push(pad_line_to_width(spans, width));
            }
            Ok(RenderedGutter { width, rows })
        }
    }
}

/// Decorator producer. Returns a flat list of styled ranges to apply across
/// the buffer.
pub fn produce_decorator(
    slot: &Slot,
    buf: &Buffer,
    theme: &Theme,
    env: &Env,
) -> Result<DecoratorRanges, RizzError> {
    match &slot.renderable {
        LispRenderable::Builtin(b) => Ok(builtin_decorator(*b, buf, theme)),
        LispRenderable::Static(v) => Ok(DecoratorRanges {
            ranges: ranges_from_value(v, theme)?,
        }),
        LispRenderable::Callable(f) => {
            let v = runtime::apply(f, &[], env)?;
            Ok(DecoratorRanges {
                ranges: ranges_from_value(&v, theme)?,
            })
        }
    }
}

/// Bottom-strip component producer. Returns one inner `Vec<Span>` per row.
pub fn produce_bottom(
    slot: &Slot,
    snap: &StateSnapshot<'_>,
    theme: &Theme,
    env: &Env,
) -> Result<Vec<Vec<Span<'static>>>, RizzError> {
    let _ = snap; // no builtin bottom components yet
    match &slot.renderable {
        LispRenderable::Builtin(_) => Ok(vec![]),
        LispRenderable::Static(v) => Ok(rows_from_value(v, theme)?),
        LispRenderable::Callable(f) => {
            let v = runtime::apply(f, &[], env)?;
            Ok(rows_from_value(&v, theme)?)
        }
    }
}

// --- builtin status segments ---

fn builtin_status_segment(
    b: BuiltinId,
    snap: &StateSnapshot<'_>,
    _theme: &Theme,
) -> Vec<Span<'static>> {
    let s = match b {
        BuiltinId::ModeGlyph => match snap.focused().mode() {
            EditingMode::Insert => "i",
            EditingMode::Normal => "n",
            EditingMode::Visual => "v",
            EditingMode::VisualLine => "V",
            EditingMode::VisualBlock => "^V",
            EditingMode::Command => "c",
        }
        .to_string(),
        BuiltinId::LastKey => snap
            .keyevent
            .as_ref()
            .map(|e| e.code.to_string())
            .unwrap_or_else(|| "None".to_string()),
        BuiltinId::BufferNo => snap.bufno.to_string(),
        _ => return vec![],
    };
    vec![Span::raw(s)]
}

// --- builtin gutter ---

fn builtin_gutter(b: BuiltinId, buf: &Buffer, _theme: &Theme) -> RenderedGutter {
    match b {
        BuiltinId::LineNumbers => {
            let max = buf.len_lines().max(1);
            let digits = ((max as f64).log10().floor() as u16) + 1;
            let width = digits.max(2) + 1;
            let w = (width - 1) as usize;

            let start = buf.file_pos().row.min(buf.len_lines());
            let visible = buf.viewport.row as usize;
            let last = buf.len_lines().saturating_sub(1);

            let mut rows = Vec::with_capacity(visible);
            for r in 0..visible {
                let lnum = start + r;
                let text = if lnum <= last {
                    format!("{:>w$} ", lnum, w = w)
                } else {
                    " ".repeat(w + 1)
                };
                rows.push(Line::from(Span::raw(text)));
            }
            RenderedGutter { width, rows }
        }
        _ => RenderedGutter {
            width: 0,
            rows: vec![],
        },
    }
}

// --- builtin decorators ---

fn builtin_decorator(b: BuiltinId, buf: &Buffer, theme: &Theme) -> DecoratorRanges {
    let start = buf.file_pos().row.min(buf.len_lines());
    let visible = buf.viewport.row as usize;

    let mut ranges = Vec::new();
    match b {
        BuiltinId::BaseFg => {
            // Apply the `default` face's full style across visible lines.
            // If the theme doesn't define `default`, this is a no-op — the
            // frame-wide base fill in the renderer already provides the
            // editor's baseline colors.
            let Some(style) = theme.resolve("default") else {
                return DecoratorRanges { ranges };
            };
            for (i, line) in buf.lines_at(start).take(visible).enumerate() {
                let text = line.to_string();
                let len = text.trim_end_matches(['\n', '\r']).chars().count();
                ranges.push(StyledRange {
                    row: start + i,
                    col: 0,
                    len,
                    style: style.clone(),
                    pad_to_width: false,
                    display: None,
                });
            }
        }
        BuiltinId::SelectionHighlight => {
            let style = theme.resolve("region").unwrap_or_else(|| Style {
                bg: Some(crate::styling::Color::Rgb(60, 90, 130)),
                ..Default::default()
            });
            let Some(anchor) = buf.selection_anchor() else {
                return DecoratorRanges { ranges };
            };
            let cur = buf.abs_pos();
            let mode = buf.mode();
            if !mode.is_visual() {
                return DecoratorRanges { ranges };
            }
            let (min_row, max_row) = order(anchor.row, cur.row);
            for (i, line) in buf.lines_at(start).take(visible).enumerate() {
                let lnum = start + i;
                if lnum < min_row || lnum > max_row {
                    continue;
                }
                let text = line.to_string();
                let line_len = text.trim_end_matches(['\n', '\r']).chars().count();
                let (col, len, pad) = match mode {
                    EditingMode::VisualLine => (0usize, line_len.max(1), true),
                    EditingMode::VisualBlock => {
                        let (lo, hi) = order(anchor.col, cur.col);
                        (lo, hi.saturating_sub(lo) + 1, false)
                    }
                    EditingMode::Visual => {
                        let (lo_row, lo_col, hi_row, hi_col) =
                            if (anchor.row, anchor.col) <= (cur.row, cur.col) {
                                (anchor.row, anchor.col, cur.row, cur.col)
                            } else {
                                (cur.row, cur.col, anchor.row, anchor.col)
                            };
                        let s = if lnum == lo_row { lo_col } else { 0 };
                        let e = if lnum == hi_row {
                            hi_col + 1
                        } else {
                            line_len.max(1)
                        };
                        (s, e.saturating_sub(s), false)
                    }
                    _ => continue,
                };
                if len == 0 {
                    continue;
                }
                ranges.push(StyledRange {
                    row: lnum,
                    col,
                    len,
                    style: style.clone(),
                    pad_to_width: pad,
                    display: None,
                });
            }
        }
        BuiltinId::CurrentLineHighlight => {
            let style = theme.resolve("cursor-line").unwrap_or_else(|| Style {
                bg: Some(crate::styling::Color::DarkGray),
                ..Default::default()
            });
            let cur_row = buf.file_pos().row + buf.cursor_pos().row as usize;
            ranges.push(StyledRange {
                row: cur_row,
                col: 0,
                len: 0,
                style,
                pad_to_width: true,
                display: None,
            });
        }
        _ => {}
    }
    DecoratorRanges { ranges }
}

fn order(a: usize, b: usize) -> (usize, usize) {
    if a <= b { (a, b) } else { (b, a) }
}

// --- value -> ranges ---
//
// Decorator slots return a sequence of `{row col len style pad}` maps.
// Each map's keys are strings (per the project-wide convention).

fn ranges_from_value(v: &Rc<Value>, theme: &Theme) -> Result<Vec<StyledRange>, RizzError> {
    let mut out = Vec::new();
    for entry in entries(v) {
        let Value::Map(m) = &*entry else {
            return Err(RizzError::from(rizz::runtime::RuntimeError::type_mismatch(
                "decorator",
                "map with row|col|len|style fields",
                &entry,
            )));
        };
        let key = |k: &str| Rc::new(Value::Str(k.into()));
        let int_field = |name: &str| -> Result<usize, RizzError> {
            let v = m.get(&key(name)).cloned().unwrap_or(Rc::new(Value::Int(0)));
            let n = v.as_int().ok_or_else(|| {
                rizz::runtime::RuntimeError::type_mismatch("decorator", "int", &v)
            })?;
            Ok(n.max(0) as usize)
        };
        let row = int_field("row")?;
        let col = int_field("col")?;
        let len = int_field("len")?;
        let style = match m.get(&key("style")) {
            Some(s) => style_from_value(s, theme)?,
            None => Style::default(),
        };
        let pad_to_width = m
            .get(&key("pad-to-width"))
            .map(|v| v.is_truthy())
            .unwrap_or(false);
        out.push(StyledRange {
            row,
            col,
            len,
            style,
            pad_to_width,
            display: None,
        });
    }
    Ok(out)
}

// --- value -> bottom rows ---
//
// A bottom component returns an array (or list) of "lines"; each line is in
// turn anything `spans_from_value` accepts. So `[(span "x") "y" {"text":"z"}]`
// is a three-row component.

fn rows_from_value(v: &Rc<Value>, theme: &Theme) -> Result<Vec<Vec<Span<'static>>>, RizzError> {
    let mut out = Vec::new();
    for row in entries(v) {
        out.push(spans_from_value(&row, theme)?);
    }
    Ok(out)
}

/// Iterate entries of either an `Array` or a `Cons` list. Rizz's
/// [`Value::iter`] only walks cons chains; arrays would otherwise be
/// returned as a single opaque entry.
fn entries(v: &Rc<Value>) -> Box<dyn Iterator<Item = Rc<Value>> + '_> {
    match &**v {
        Value::Array(xs) => Box::new(xs.iter().cloned().collect::<Vec<_>>().into_iter()),
        _ => Box::new(Value::iter(v)),
    }
}

// --- helpers ---

fn pad_line_to_width(spans: Vec<Span<'static>>, width: u16) -> Line<'static> {
    use unicode_width::UnicodeWidthStr;
    let used: usize = spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum();
    let mut spans = spans;
    if (width as usize) > used {
        spans.push(Span::raw(" ".repeat(width as usize - used)));
    }
    Line::from(spans)
}

#[allow(dead_code)] // exposed for default-style.lisp callers in commit 5
pub fn default_face_rgb_bg() -> Rc<Value> {
    rgb_value(60, 90, 130)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(name: &str, kind: SlotKind, b: BuiltinId) -> Slot {
        Slot {
            name: name.into(),
            kind,
            renderable: LispRenderable::Builtin(b),
        }
    }

    #[test]
    fn builtin_parse_round_trips() {
        assert_eq!(
            BuiltinId::parse("line-numbers"),
            Some(BuiltinId::LineNumbers)
        );
        assert_eq!(
            BuiltinId::parse("current-line-highlight"),
            Some(BuiltinId::CurrentLineHighlight)
        );
        assert_eq!(BuiltinId::parse("no-such-thing"), None);
    }

    #[test]
    fn builtin_category_matches_kind() {
        assert_eq!(BuiltinId::LineNumbers.category(), SlotCategory::Gutter);
        assert_eq!(BuiltinId::BaseFg.category(), SlotCategory::Decorator);
        assert_eq!(BuiltinId::ModeGlyph.category(), SlotCategory::StatusSegment);
    }

    #[test]
    fn add_then_remove_slot() {
        let mut r = SlotRegistry::new();
        r.add(slot(
            "lnum",
            SlotKind::Gutter { width: 4 },
            BuiltinId::LineNumbers,
        ));
        assert_eq!(r.gutters().len(), 1);
        assert!(r.remove(SlotCategory::Gutter, "lnum"));
        assert_eq!(r.gutters().len(), 0);
        assert!(!r.remove(SlotCategory::Gutter, "lnum"));
    }

    #[test]
    fn add_with_existing_name_replaces_in_place() {
        let mut r = SlotRegistry::new();
        r.add(slot("a", SlotKind::Decorator, BuiltinId::BaseFg));
        r.add(slot(
            "b",
            SlotKind::Decorator,
            BuiltinId::CurrentLineHighlight,
        ));
        // Re-add "a" with a different builtin; should replace in place.
        r.add(slot(
            "a",
            SlotKind::Decorator,
            BuiltinId::SelectionHighlight,
        ));
        let ds = r.decorators();
        assert_eq!(ds.len(), 2);
        assert_eq!(ds[0].name.as_ref(), "a");
        assert!(matches!(
            ds[0].renderable,
            LispRenderable::Builtin(BuiltinId::SelectionHighlight)
        ));
    }

    #[test]
    fn replace_swaps_entire_list() {
        let mut r = SlotRegistry::new();
        r.add(slot("a", SlotKind::Decorator, BuiltinId::BaseFg));
        r.add(slot(
            "b",
            SlotKind::Decorator,
            BuiltinId::CurrentLineHighlight,
        ));
        r.replace(
            SlotCategory::Decorator,
            None,
            vec![slot(
                "c",
                SlotKind::Decorator,
                BuiltinId::SelectionHighlight,
            )],
        );
        let ds = r.decorators();
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].name.as_ref(), "c");
    }

    #[test]
    fn status_segments_are_per_side() {
        let mut r = SlotRegistry::new();
        r.add(slot(
            "mode",
            SlotKind::StatusSegment {
                side: SegmentSide::Left,
            },
            BuiltinId::ModeGlyph,
        ));
        r.add(slot(
            "key",
            SlotKind::StatusSegment {
                side: SegmentSide::Right,
            },
            BuiltinId::LastKey,
        ));
        assert_eq!(r.status_segments(SegmentSide::Left).len(), 1);
        assert_eq!(r.status_segments(SegmentSide::Right).len(), 1);
        assert!(r.remove(SlotCategory::StatusSegment, "mode"));
        assert_eq!(r.status_segments(SegmentSide::Left).len(), 0);
        assert_eq!(r.status_segments(SegmentSide::Right).len(), 1);
    }
}
