//! Build a [`RenderedFrame`] from the editor's current state: snapshot the
//! theme, evaluate the user's `set-frame` fn into a [`Widget`] tree,
//! pre-render gutters and decorator ranges for each buffer, and build
//! soft-wrap layouts for opt-in buffers. The theme is snapshotted up front
//! so a `face-define` from inside a render callback only affects the next
//! frame.

use std::rc::Rc;

use ratatui::text::{Line, Span};
use rizz::Env;
use rizz::runtime::{self, Value};

use rizz_core::EditingMode;
use rizz_text::{
    Buffer, BufferId,
    props::{PropEntry, PropStore},
    wrap::{WrapConfig, WrapMap, WrapMode},
};
use slotmap::{SecondaryMap, SlotMap};

use crate::render::{
    DecoratorRanges, GutterWidth, RenderedBuffer, RenderedFrame, RenderedGutter, StyledRange,
};
use crate::styling::{Color, Style, Theme, ThemeCell, spans_from_value, style_from_value};
use crate::widget::{Widget, parse_widget};
use crate::window::WindowTree;

/// Inputs the precompute pass reads from `State`. All references are
/// immutable; the only mutation it performs is on its own local builders.
pub struct PrecomputeInput<'a> {
    pub bufs: &'a SlotMap<BufferId, Buffer>,
    pub windows: &'a WindowTree,
    pub frame_fn: Option<&'a Rc<Value>>,
    pub theme: &'a ThemeCell,
    /// Skipped from decorator passes — the minibuffer is plain text.
    pub minibuffer: BufferId,
    /// File buffers (cycled via `:bn`/`:bp`). Only these get a gutter;
    /// popup-backing buffers don't.
    pub file_bufs: &'a [BufferId],
    /// `None` means "no gutter".
    pub gutter: Option<&'a Rc<Value>>,
    pub gutter_width: GutterWidth,
    pub lisp_env: &'a Env,
}

pub fn compute(input: PrecomputeInput<'_>) -> (RenderedFrame, Option<String>) {
    let PrecomputeInput {
        bufs,
        windows: _,
        frame_fn,
        theme,
        minibuffer,
        file_bufs,
        gutter,
        gutter_width,
        lisp_env,
    } = input;

    let theme_snap = theme.borrow().clone();
    let default_style = theme_snap.resolve("default").unwrap_or_default();

    let mut errors: Vec<String> = Vec::new();
    let record = |errs: &mut Vec<String>, ctx: &str, msg: String| {
        if errs.len() < 3 {
            errs.push(format!("[{ctx}] {msg}"));
        }
    };

    let mut root = match frame_fn {
        Some(f) => match runtime::apply(f, &[], lisp_env) {
            Ok(v) => match parse_widget(&v, &theme_snap) {
                Ok(w) => w,
                Err(e) => {
                    record(&mut errors, "frame", e.to_string());
                    default_layout()
                }
            },
            Err(e) => {
                record(&mut errors, "frame", e.to_string());
                default_layout()
            }
        },
        None => default_layout(),
    };

    let mut per_buf: SecondaryMap<BufferId, RenderedBuffer> = SecondaryMap::new();
    for (id, buf) in bufs.iter() {
        let mut rb = RenderedBuffer::default();

        let is_file = file_bufs.contains(&id);
        let is_minibuffer = id == minibuffer;

        if is_file && gutter.is_some() && !matches!(gutter_width, GutterWidth::Fixed(0)) {
            let g = build_gutter(buf, gutter_width, gutter, &theme_snap, lisp_env);
            match g {
                Ok(g) => rb.gutter = Some(g),
                Err(e) => record(&mut errors, "gutter", e),
            }
        }

        if !is_minibuffer {
            push_builtin_decorators(buf, &theme_snap, &mut rb);
        }

        if !is_minibuffer {
            let prop_ranges = build_prop_ranges(buf, &theme_snap);
            if !prop_ranges.ranges.is_empty() {
                rb.decorators.push(prop_ranges);
            }
        }

        if !matches!(buf.wrap_mode(), WrapMode::None) && buf.viewport.row > 0 {
            let gutter_w: u16 = rb.gutter.as_ref().map(|g| g.width).unwrap_or(0);
            let content_w = buf
                .wrap_column()
                .unwrap_or_else(|| buf.viewport.col.saturating_sub(gutter_w));
            if content_w > 0 {
                let cfg = WrapConfig {
                    mode: buf.wrap_mode(),
                    width: content_w,
                    breakindent: buf.breakindent(),
                };
                let budget = ((buf.viewport.row as usize) * 4).max(200);
                let map = WrapMap::build(buf, buf.file_pos().row, budget, cfg);
                rb.wrap = Some(map);
            }
        }

        per_buf.insert(id, rb);
    }

    let _ = &mut root;

    let error_msg = if errors.is_empty() {
        None
    } else {
        Some(errors.join("; "))
    };

    (
        RenderedFrame {
            default_style,
            theme: theme_snap,
            root,
            per_buf,
        },
        error_msg,
    )
}

/// Fallback layout when no `set-frame` fn is installed (or one errored).
/// Renders editor windows + minibuffer with no gutter and no status line.
fn default_layout() -> Widget {
    use crate::widget::{ConstraintKind, StackDir};
    Widget::Stack {
        dir: StackDir::Vertical,
        children: vec![
            Widget::Constrained {
                kind: ConstraintKind::Min,
                n: 1,
                m: 1,
                child: Box::new(Widget::EditorTree),
            },
            Widget::Constrained {
                kind: ConstraintKind::Cells,
                n: 1,
                m: 1,
                child: Box::new(Widget::Minibuffer),
            },
        ],
    }
}

fn build_gutter(
    buf: &Buffer,
    width: GutterWidth,
    gutter_fn: Option<&Rc<Value>>,
    theme: &Theme,
    env: &Env,
) -> Result<RenderedGutter, String> {
    use unicode_width::UnicodeWidthStr;

    let start = buf.file_pos().row.min(buf.len_lines());
    let visible = buf.viewport.row as usize;
    let last = buf.len_lines().saturating_sub(1);

    let mut raw: Vec<(Vec<Span<'static>>, usize)> = Vec::with_capacity(visible);
    for r in 0..visible {
        let lnum = start + r;
        let lnum_arg: Rc<Value> = if lnum <= last {
            Rc::new(Value::Int(lnum as i64))
        } else {
            Rc::new(Value::Unit)
        };
        let v = match gutter_fn {
            Some(f) => runtime::apply(f, &[lnum_arg], env).map_err(|e| e.to_string())?,
            None => Rc::new(Value::Unit),
        };
        let spans = spans_from_value(&v, theme).map_err(|e| e.to_string())?;
        let used: usize = spans
            .iter()
            .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
            .sum();
        raw.push((spans, used));
    }

    let resolved: u16 = match width {
        GutterWidth::Fixed(n) => n,
        GutterWidth::Fit => raw
            .iter()
            .map(|(_, used)| *used)
            .max()
            .unwrap_or(0)
            .min(u16::MAX as usize) as u16,
    };

    let rows = raw
        .into_iter()
        .map(|(spans, used)| pad_line_to_width(spans, used, resolved))
        .collect();
    Ok(RenderedGutter {
        width: resolved,
        rows,
    })
}

fn pad_line_to_width(spans: Vec<Span<'static>>, used: usize, width: u16) -> Line<'static> {
    let mut spans = spans;
    if (width as usize) > used {
        spans.push(Span::raw(" ".repeat(width as usize - used)));
    }
    Line::from(spans)
}

/// Order matters: later passes paint over earlier ones. Base-fg first,
/// then optional syntax/diagnostics, then cursor-line and selection
/// backgrounds on top.
fn push_builtin_decorators(buf: &Buffer, theme: &Theme, rb: &mut RenderedBuffer) {
    rb.decorators.push(base_fg_ranges(buf, theme));
    let syntax = syntax_ranges(buf, theme);
    if !syntax.ranges.is_empty() {
        rb.decorators.push(syntax);
    }
    let diagnostics = diagnostic_ranges(buf, theme);
    if !diagnostics.ranges.is_empty() {
        rb.decorators.push(diagnostics);
    }
    rb.decorators.push(current_line_ranges(buf, theme));
    rb.decorators.push(selection_ranges(buf, theme));
}

/// Styled ranges for every LSP diagnostic in the viewport. Faces are
/// `diagnostic.error`, `diagnostic.warning`, etc. (see
/// `rizz_core::Severity::face`).
fn diagnostic_ranges(buf: &Buffer, theme: &Theme) -> DecoratorRanges {
    let mut ranges = Vec::new();
    let diags = buf.diagnostics();
    if diags.is_empty() {
        return DecoratorRanges { ranges };
    }
    let start_row = buf.file_pos().row.min(buf.len_lines());
    let visible = buf.viewport.row as usize;
    if visible == 0 {
        return DecoratorRanges { ranges };
    }
    let end_row_excl = (start_row + visible).min(buf.len_lines() + 1);
    for d in diags {
        let Some(style) = theme.resolve(d.severity.face()) else {
            continue;
        };
        let s_row = d.start.row;
        let e_row = d.end.row;
        let first_row = s_row.max(start_row);
        let last_row = e_row.min(end_row_excl.saturating_sub(1));
        if first_row > last_row {
            continue;
        }
        for row in first_row..=last_row {
            let col = if row == s_row { d.start.col } else { 0 };
            let row_line_len = line_char_count(buf, row);
            let end_col = if row == e_row {
                d.end.col.min(row_line_len)
            } else {
                row_line_len
            };
            let len = end_col.saturating_sub(col);
            if len == 0 {
                continue;
            }
            ranges.push(StyledRange {
                row,
                col,
                len,
                style: style.clone(),
                pad_to_width: false,
                display: None,
            });
        }
    }
    DecoratorRanges { ranges }
}

/// Styled ranges for tree-sitter captures in the viewport.
///
/// Pre-condition: the highlighter's tree must be in sync with the rope —
/// `State::precompute_frame` calls [`Buffer::refresh_highlight`] before
/// this runs.
fn syntax_ranges(buf: &Buffer, theme: &Theme) -> DecoratorRanges {
    let mut ranges = Vec::new();
    let Some(h) = buf.highlight() else {
        return DecoratorRanges { ranges };
    };
    let start_row = buf.file_pos().row.min(buf.len_lines());
    let visible = buf.viewport.row as usize;
    if visible == 0 {
        return DecoratorRanges { ranges };
    }
    let end_row_excl = (start_row + visible).min(buf.len_lines() + 1);

    let rope = buf.rope();
    let len_bytes = rope.len_bytes();
    let start_byte = if start_row < rope.len_lines() {
        rope.line_to_byte(start_row)
    } else {
        len_bytes
    };
    let end_byte = if end_row_excl < rope.len_lines() {
        rope.line_to_byte(end_row_excl)
    } else {
        len_bytes
    };
    if start_byte >= end_byte {
        return DecoratorRanges { ranges };
    }

    for span in h.query(start_byte, end_byte) {
        let Some(style) = resolve_syntax_face(theme, &span.capture) else {
            continue;
        };
        // tree-sitter byte offsets → rope char coords.
        let s_char = rope.byte_to_char(span.start_byte);
        let e_char = rope.byte_to_char(span.end_byte);
        let s_row = rope.char_to_line(s_char);
        let e_row = rope.char_to_line(e_char);
        let s_line_start = rope.line_to_char(s_row);
        let s_col = s_char - s_line_start;
        let last_row = e_row.min(end_row_excl.saturating_sub(1));
        let first_row = s_row.max(start_row);
        if first_row > last_row {
            continue;
        }
        for row in first_row..=last_row {
            let line_start = rope.line_to_char(row);
            let col = if row == s_row { s_col } else { 0 };
            let row_line_len = line_char_count(buf, row);
            let end_col = if row == e_row {
                e_char - line_start
            } else {
                row_line_len
            };
            let len = end_col.saturating_sub(col);
            if len == 0 {
                continue;
            }
            ranges.push(StyledRange {
                row,
                col,
                len,
                style: style.clone(),
                pad_to_width: false,
                display: None,
            });
        }
    }
    DecoratorRanges { ranges }
}

fn base_fg_ranges(buf: &Buffer, theme: &Theme) -> DecoratorRanges {
    let mut ranges = Vec::new();
    let Some(style) = theme.resolve("default") else {
        return DecoratorRanges { ranges };
    };
    let start = buf.file_pos().row.min(buf.len_lines());
    let visible = buf.viewport.row as usize;
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
    DecoratorRanges { ranges }
}

fn current_line_ranges(buf: &Buffer, theme: &Theme) -> DecoratorRanges {
    let style = theme.resolve("cursor-line").unwrap_or_else(|| Style {
        bg: Some(Color::DarkGray),
        ..Default::default()
    });
    let cur_row = buf.abs_row();
    DecoratorRanges {
        ranges: vec![StyledRange {
            row: cur_row,
            col: 0,
            len: 0,
            style,
            pad_to_width: true,
            display: None,
        }],
    }
}

fn selection_ranges(buf: &Buffer, theme: &Theme) -> DecoratorRanges {
    let mut ranges = Vec::new();
    let style = theme.resolve("selection").unwrap_or_else(|| Style {
        bg: Some(Color::Rgb(60, 90, 130)),
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
    let start = buf.file_pos().row.min(buf.len_lines());
    let visible = buf.viewport.row as usize;
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
    DecoratorRanges { ranges }
}

/// Render-ready [`StyledRange`]s from the buffer's text properties and
/// overlays, clipped to the visible viewport.
pub fn build_prop_ranges(buf: &Buffer, theme: &Theme) -> DecoratorRanges {
    let mut ranges = Vec::new();
    let start_row = buf.file_pos().row;
    let visible_rows = buf.viewport.row as usize;
    if visible_rows == 0 {
        return DecoratorRanges { ranges };
    }
    let end_row_excl = start_row + visible_rows;

    let store: &PropStore = buf.props();

    for e in &store.text_properties {
        emit_clipped(e, buf, theme, start_row, end_row_excl, &mut ranges);
    }

    let mut ov: Vec<&PropEntry> = store.overlays.iter().map(|(_, e)| e).collect();
    ov.sort_by_key(|e| e.priority);
    for e in ov {
        emit_clipped(e, buf, theme, start_row, end_row_excl, &mut ranges);
    }

    DecoratorRanges { ranges }
}

fn emit_clipped(
    e: &PropEntry,
    buf: &Buffer,
    theme: &Theme,
    viewport_start: usize,
    viewport_end_excl: usize,
    out: &mut Vec<StyledRange>,
) {
    let Some(face_val) = e.face.as_ref() else {
        return;
    };
    let Ok(style) = style_from_value(face_val, theme) else {
        return;
    };

    let lo_row = e.start.row.max(viewport_start);
    let hi_row = e.end.row.min(viewport_end_excl.saturating_sub(1));
    if lo_row > hi_row {
        return;
    }

    for row in lo_row..=hi_row {
        let line_len = line_char_count(buf, row);
        let col = if row == e.start.row { e.start.col } else { 0 };
        let end_col = if row == e.end.row {
            e.end.col
        } else {
            line_len
        };
        let len = end_col.saturating_sub(col);
        let display = if row == e.start.row && e.start.row == e.end.row {
            e.display.clone()
        } else {
            None
        };
        if len == 0 && !e.pad_to_width && display.is_none() {
            continue;
        }
        out.push(StyledRange {
            row,
            col,
            len,
            style: style.clone(),
            pad_to_width: e.pad_to_width,
            display,
        });
    }
}

/// Resolve a tree-sitter capture's face. Tries the fully-qualified name
/// (`syntax.function.method`), then peels off dotted suffixes
/// (`syntax.function`, `syntax`) so a theme defining only the base
/// category still styles every refinement.
fn resolve_syntax_face(theme: &Theme, capture: &str) -> Option<Style> {
    let mut name: &str = capture;
    loop {
        let face = format!("syntax.{name}");
        if let Some(style) = theme.resolve(&face) {
            return Some(style);
        }
        match name.rfind('.') {
            Some(i) => name = &name[..i],
            None => return None,
        }
    }
}

fn line_char_count(buf: &Buffer, row: usize) -> usize {
    if buf.len_lines() < row {
        return 0;
    }
    buf.lines_at(row)
        .next()
        .map(|line| {
            line.to_string()
                .trim_end_matches(['\n', '\r'])
                .chars()
                .count()
        })
        .unwrap_or(0)
}

fn order(a: usize, b: usize) -> (usize, usize) {
    if a <= b { (a, b) } else { (b, a) }
}
