//! Build a [`RenderedFrame`] from the editor's current state.
//!
//! Pipeline:
//!
//! 1. Snapshot the theme (so a `face-define` from inside a render callback
//!    only affects the next frame).
//! 2. Evaluate the user's `set-frame` fn — its return value is parsed into a
//!    [`Widget`] tree.
//! 3. For each `EditorTree` node found in the tree, pre-render the gutter
//!    rows for every editor buffer using the node's `gutter` callback.
//! 4. Build per-buffer decorator ranges: built-in `base-fg`, selection
//!    highlight, current-line highlight, plus the buffer's `prop_store`
//!    (text properties + overlays).
//! 5. Build soft-wrap layouts for buffers that opt in.

use std::rc::Rc;

use ratatui::text::{Line, Span};
use rizz::Env;
use rizz::runtime::{self, Value};

use crate::buffer::{Buffer, BufferKind};
use crate::mode::EditingMode;
use crate::ui::render::{
    DecoratorRanges, RenderedBuffer, RenderedFrame, RenderedGutter, StyledRange,
};
use crate::ui::styling::{Color, Style, Theme, ThemeCell, spans_from_value};
use crate::ui::widget::{Widget, parse_widget};
use crate::ui::window::WindowTree;

/// Inputs the precompute pass reads from `State`. All references are
/// immutable; the only mutation it performs is on its own local builders.
pub struct PrecomputeInput<'a> {
    pub bufs: &'a [Buffer],
    pub windows: &'a WindowTree,
    pub frame_fn: Option<&'a Rc<Value>>,
    pub theme: &'a ThemeCell,
    pub minibuffer: usize,
    pub lisp_env: &'a Env,
}

pub fn compute(input: PrecomputeInput<'_>) -> (RenderedFrame, Option<String>) {
    let PrecomputeInput {
        bufs,
        windows: _,
        frame_fn,
        theme,
        minibuffer,
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

    // 1. Build the widget tree by calling the user's frame fn (if any).
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

    // 2. Discover EditorTree nodes and capture their gutter spec. We support
    //    one effective spec per frame — the first EditorTree we encounter
    //    wins. (Multiple editor-tree nodes in one layout is unusual.)
    let (gutter_width, gutter_fn) = find_editor_tree_spec(&root);

    // 3. Per-buffer precompute: gutter rows, decorator ranges, wrap layout.
    let mut per_buf = Vec::with_capacity(bufs.len());
    for (i, buf) in bufs.iter().enumerate() {
        let mut rb = RenderedBuffer::default();

        let is_visible_editor = i != minibuffer && buf.kind() == BufferKind::File;

        if is_visible_editor {
            // Gutter rows. Built once, applied uniformly to every leaf that
            // points at this buffer (the same buffer in two splits gets the
            // same gutter — fine, since gutter content is buffer-local).
            if gutter_width > 0 {
                let g = build_gutter(buf, gutter_width, gutter_fn.as_ref(), &theme_snap, lisp_env);
                match g {
                    Ok(g) => rb.gutter = Some(g),
                    Err(e) => record(&mut errors, "gutter", e),
                }
            }
        }

        // Built-in decorator passes: base-fg, selection, current-line.
        // Run for editor buffers and popup buffers (popups still want
        // selection / cursor-line highlights inside their content).
        if buf.kind() != BufferKind::Minibuffer {
            push_builtin_decorators(buf, &theme_snap, &mut rb);
        }

        // Buffer-attached text properties + overlays.
        if i != minibuffer {
            let prop_ranges = crate::ui::props::build_prop_ranges(buf, &theme_snap);
            if !prop_ranges.ranges.is_empty() {
                rb.decorators.push(prop_ranges);
            }
        }

        // Soft-wrap layout. Built after gutters so the wrap width is the
        // actual content area (viewport - gutters).
        if !matches!(buf.wrap_mode(), crate::ui::wrap::WrapMode::None) && buf.viewport.row > 0 {
            let gutter_w: u16 = rb.gutter.as_ref().map(|g| g.width).unwrap_or(0);
            let content_w = buf
                .wrap_column()
                .unwrap_or_else(|| buf.viewport.col.saturating_sub(gutter_w));
            if content_w > 0 {
                let cfg = crate::ui::wrap::WrapConfig {
                    mode: buf.wrap_mode(),
                    width: content_w,
                    breakindent: buf.breakindent(),
                };
                let budget = ((buf.viewport.row as usize) * 4).max(200);
                let map = crate::ui::wrap::WrapMap::build(buf, buf.file_pos().row, budget, cfg);
                rb.wrap = Some(map);
            }
        }

        per_buf.push(rb);
    }

    // 4. Mutate the tree so EditorTree widgets carry their effective width
    //    (already set during parsing; we just preserve the structure).
    // No-op: we already parsed gutter_width into Widget::EditorTree.
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

/// The fallback layout used when no `set-frame` fn has been installed
/// (or when one errors out). Renders editor windows + minibuffer with no
/// gutter, no status line.
fn default_layout() -> Widget {
    use crate::ui::widget::{ConstraintKind, StackDir};
    Widget::Stack {
        dir: StackDir::Vertical,
        children: vec![
            Widget::Constrained {
                kind: ConstraintKind::Min,
                n: 1,
                m: 1,
                child: Box::new(Widget::EditorTree {
                    gutter_width: 0,
                    gutter: None,
                }),
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

/// Walk the tree and return the first `EditorTree` node's gutter spec.
fn find_editor_tree_spec(w: &Widget) -> (u16, Option<Rc<Value>>) {
    match w {
        Widget::EditorTree {
            gutter_width,
            gutter,
        } => (*gutter_width, gutter.clone()),
        Widget::Stack { children, .. } => {
            for c in children {
                let (w, f) = find_editor_tree_spec(c);
                if w > 0 || f.is_some() {
                    return (w, f);
                }
                // Even when both are zero/None, if we encountered an
                // EditorTree at all, we should return — but we can't tell
                // that here. The caller treats (0, None) as "no gutter".
            }
            (0, None)
        }
        Widget::Constrained { child, .. } => find_editor_tree_spec(child),
        Widget::Block { child, .. } => find_editor_tree_spec(child),
        _ => (0, None),
    }
}

fn build_gutter(
    buf: &Buffer,
    width: u16,
    gutter_fn: Option<&Rc<Value>>,
    theme: &Theme,
    env: &Env,
) -> Result<RenderedGutter, String> {
    let start = buf.file_pos().row.min(buf.len_lines());
    let visible = buf.viewport.row as usize;
    let last = buf.len_lines().saturating_sub(1);

    let mut rows = Vec::with_capacity(visible);
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
        rows.push(pad_line_to_width(spans, width));
    }
    Ok(RenderedGutter { width, rows })
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

/// Always-on decorator passes that used to live as `BuiltinId` regions:
/// base-fg, selection highlight, current-line highlight. Pushed in that
/// order so cursor-line layers under selection layers under text content
/// (selection takes precedence on selected rows).
fn push_builtin_decorators(buf: &Buffer, theme: &Theme, rb: &mut RenderedBuffer) {
    rb.decorators.push(base_fg_ranges(buf, theme));
    rb.decorators.push(current_line_ranges(buf, theme));
    rb.decorators.push(selection_ranges(buf, theme));
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

fn order(a: usize, b: usize) -> (usize, usize) {
    if a <= b { (a, b) } else { (b, a) }
}
