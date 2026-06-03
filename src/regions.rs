//! Regions: named, positioned content producers.
//!
//! A [`Region`] is a producer of rendered content attached to a screen
//! position via [`RegionAnchor`]. This replaces the older 4-category slot
//! model (status segment / gutter / decorator / bottom) with a single
//! registry: every customization point is a region, the anchor decides where
//! and how it appears on screen.
//!
//! The precompute pass in `state.rs` walks the registry once per frame and
//! routes each region's output to the right slot in [`RenderedFrame`].
//!
//! Anchors and the producer return shape they expect:
//!
//! | Anchor          | Producer return                        |
//! |-----------------|----------------------------------------|
//! | `Top`           | array of rows (each row = array of spans) |
//! | `Bottom`        | array of rows                          |
//! | `StatusLeft`    | array of spans                         |
//! | `StatusRight`   | array of spans                         |
//! | `Gutter`        | one styled value per visible row       |
//! | `Decorator`     | array of `{row col len style ...}` maps |
//!
//! Three producer flavors:
//!
//! * [`Producer::Static`]   — a literal lisp value, consumed verbatim.
//! * [`Producer::Callable`] — a closure / native fn called each frame.
//! * [`Producer::Builtin`]  — a built-in Rust impl identified by
//!   [`BuiltinId`]. The bundled theme uses these to reuse the optimized
//!   built-in producers (line numbers, base-fg, selection-highlight, etc.).

use std::rc::Rc;

use ratatui::text::{Line, Span};
use rizz::RizzError;
use rizz::runtime::{self, Env, Value};

use crate::buffer::Buffer;
use crate::mode::EditingMode;
use crate::render::{DecoratorRanges, RenderedGutter, StateSnapshot, StyledRange};
use crate::styling::{Style, Theme, spans_from_value, style_from_value};

// ---------------------------------------------------------------------------
// Producer
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum Producer {
    Static(Rc<Value>),
    Callable(Rc<Value>),
    Builtin(BuiltinId),
}

/// Built-in Rust producers, addressable from lisp by symbol. Lets a theme
/// reuse the optimized Rust impl by name instead of recreating it in lisp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinId {
    LineNumbers,
    BaseFg,
    SelectionHighlight,
    CurrentLineHighlight,
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

    /// Whether this builtin is meaningful for the given anchor. Used by
    /// `region-add` to reject mismatches like a gutter builtin attached to a
    /// status region.
    pub fn matches_anchor(self, anchor: &RegionAnchor) -> bool {
        use BuiltinId as B;
        use RegionAnchor as A;
        matches!(
            (anchor, self),
            (A::Gutter { .. }, B::LineNumbers)
                | (
                    A::Decorator,
                    B::BaseFg | B::SelectionHighlight | B::CurrentLineHighlight
                )
                | (
                    A::StatusLeft | A::StatusRight,
                    B::ModeGlyph | B::LastKey | B::BufferNo
                )
        )
    }
}

// ---------------------------------------------------------------------------
// Region
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionAnchor {
    /// Full-width strip at the very top of the frame, above all editor
    /// windows. Multi-row — height is determined by the producer's row count.
    Top,
    /// Full-width strip between the status line and the minibuffer.
    Bottom,
    /// Single-row span in the canonical status line, left-justified.
    StatusLeft,
    /// Single-row span in the canonical status line, right-justified.
    StatusRight,
    /// Vertical column adjacent to each buffer's content area, on the left.
    /// `width` is the column count reserved in the layout.
    Gutter { width: u16 },
    /// Per-buffer content overlay: emits `StyledRange`s the renderer applies
    /// after the base text is drawn.
    Decorator,
}

#[derive(Clone, Debug)]
pub struct Region {
    pub name: Rc<str>,
    pub anchor: RegionAnchor,
    pub producer: Producer,
}

/// Ordered list of all regions. Insertion order is render order — earlier
/// regions appear first in their anchor's strip (top-to-bottom for rows,
/// left-to-right for status segments).
#[derive(Clone, Debug, Default)]
pub struct RegionRegistry {
    regions: Vec<Region>,
}

impl RegionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Region> {
        self.regions.iter()
    }

    /// Append a region. If a region with the same name already exists, it's
    /// replaced in place so re-registration is idempotent and order-stable.
    pub fn add(&mut self, region: Region) {
        match self.regions.iter().position(|r| r.name == region.name) {
            Some(i) => self.regions[i] = region,
            None => self.regions.push(region),
        }
    }

    /// Remove the region named `name`. Returns true if one was found.
    pub fn remove(&mut self, name: &str) -> bool {
        if let Some(i) = self.regions.iter().position(|r| r.name.as_ref() == name) {
            self.regions.remove(i);
            true
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Producers
// ---------------------------------------------------------------------------
//
// One per anchor return shape. Each dispatches on `Producer` (Static /
// Callable / Builtin) and returns the concrete data the renderer needs.

pub fn produce_status_span(
    region: &Region,
    snap: &StateSnapshot<'_>,
    theme: &Theme,
    env: &Env,
) -> Result<Vec<Span<'static>>, RizzError> {
    match &region.producer {
        Producer::Static(v) => Ok(spans_from_value(v, theme)?),
        Producer::Callable(f) => {
            let v = runtime::apply(f, &[], env)?;
            Ok(spans_from_value(&v, theme)?)
        }
        Producer::Builtin(b) => Ok(builtin_status_segment(*b, snap, theme)),
    }
}

pub fn produce_strip_rows(
    region: &Region,
    theme: &Theme,
    env: &Env,
) -> Result<Vec<Vec<Span<'static>>>, RizzError> {
    match &region.producer {
        Producer::Builtin(_) => Ok(vec![]),
        Producer::Static(v) => rows_from_value(v, theme),
        Producer::Callable(f) => {
            let v = runtime::apply(f, &[], env)?;
            rows_from_value(&v, theme)
        }
    }
}

pub fn produce_gutter(
    region: &Region,
    buf: &Buffer,
    theme: &Theme,
    env: &Env,
) -> Result<RenderedGutter, RizzError> {
    let RegionAnchor::Gutter {
        width: registered_width,
    } = region.anchor
    else {
        return Ok(RenderedGutter {
            width: 0,
            rows: vec![],
        });
    };
    match &region.producer {
        Producer::Builtin(b) => Ok(builtin_gutter(*b, buf, theme)),
        Producer::Static(_) | Producer::Callable(_) => {
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
                let v = match &region.producer {
                    Producer::Static(v) => v.clone(),
                    Producer::Callable(f) => runtime::apply(f, &[lnum_arg], env)?,
                    Producer::Builtin(_) => unreachable!(),
                };
                let spans = spans_from_value(&v, theme)?;
                rows.push(pad_line_to_width(spans, width));
            }
            Ok(RenderedGutter { width, rows })
        }
    }
}

pub fn produce_decorator(
    region: &Region,
    buf: &Buffer,
    theme: &Theme,
    env: &Env,
) -> Result<DecoratorRanges, RizzError> {
    match &region.producer {
        Producer::Builtin(b) => Ok(builtin_decorator(*b, buf, theme)),
        Producer::Static(v) => Ok(DecoratorRanges {
            ranges: ranges_from_value(v, theme)?,
        }),
        Producer::Callable(f) => {
            let v = runtime::apply(f, &[], env)?;
            Ok(DecoratorRanges {
                ranges: ranges_from_value(&v, theme)?,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Built-in implementations
// ---------------------------------------------------------------------------

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

fn builtin_decorator(b: BuiltinId, buf: &Buffer, theme: &Theme) -> DecoratorRanges {
    let start = buf.file_pos().row.min(buf.len_lines());
    let visible = buf.viewport.row as usize;

    let mut ranges = Vec::new();
    match b {
        BuiltinId::BaseFg => {
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
            let style = theme.resolve("selection").unwrap_or_else(|| Style {
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

// ---------------------------------------------------------------------------
// Value parsers shared across producers
// ---------------------------------------------------------------------------

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

fn rows_from_value(v: &Rc<Value>, theme: &Theme) -> Result<Vec<Vec<Span<'static>>>, RizzError> {
    let mut out = Vec::new();
    for row in entries(v) {
        out.push(spans_from_value(&row, theme)?);
    }
    Ok(out)
}

/// Iterate entries of either an `Array` or a `Cons` list. Rizz's `Value::iter`
/// only walks cons chains; arrays would otherwise be returned as a single
/// opaque entry.
fn entries(v: &Rc<Value>) -> Box<dyn Iterator<Item = Rc<Value>> + '_> {
    match &**v {
        Value::Array(xs) => Box::new(xs.iter().cloned().collect::<Vec<_>>().into_iter()),
        _ => Box::new(Value::iter(v)),
    }
}

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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn region(name: &str, anchor: RegionAnchor, b: BuiltinId) -> Region {
        Region {
            name: name.into(),
            anchor,
            producer: Producer::Builtin(b),
        }
    }

    #[test]
    fn builtin_parse_round_trips() {
        assert_eq!(
            BuiltinId::parse("line-numbers"),
            Some(BuiltinId::LineNumbers)
        );
        assert_eq!(BuiltinId::parse("no-such-thing"), None);
    }

    #[test]
    fn builtin_matches_anchor() {
        assert!(BuiltinId::LineNumbers.matches_anchor(&RegionAnchor::Gutter { width: 4 }));
        assert!(BuiltinId::BaseFg.matches_anchor(&RegionAnchor::Decorator));
        assert!(BuiltinId::ModeGlyph.matches_anchor(&RegionAnchor::StatusLeft));
        assert!(BuiltinId::ModeGlyph.matches_anchor(&RegionAnchor::StatusRight));
        assert!(!BuiltinId::ModeGlyph.matches_anchor(&RegionAnchor::Decorator));
    }

    #[test]
    fn add_then_remove() {
        let mut r = RegionRegistry::new();
        r.add(region(
            "lnum",
            RegionAnchor::Gutter { width: 4 },
            BuiltinId::LineNumbers,
        ));
        assert_eq!(r.iter().count(), 1);
        assert!(r.remove("lnum"));
        assert_eq!(r.iter().count(), 0);
        assert!(!r.remove("lnum"));
    }

    #[test]
    fn add_with_existing_name_replaces_in_place() {
        let mut r = RegionRegistry::new();
        r.add(region("a", RegionAnchor::Decorator, BuiltinId::BaseFg));
        r.add(region(
            "b",
            RegionAnchor::Decorator,
            BuiltinId::CurrentLineHighlight,
        ));
        r.add(region(
            "a",
            RegionAnchor::Decorator,
            BuiltinId::SelectionHighlight,
        ));
        let rs: Vec<&Region> = r.iter().collect();
        assert_eq!(rs.len(), 2);
        assert_eq!(rs[0].name.as_ref(), "a");
        assert!(matches!(
            rs[0].producer,
            Producer::Builtin(BuiltinId::SelectionHighlight)
        ));
    }
}
