//! Frame layout as a widget tree.
//!
//! The user's `init.lisp` registers a single function via `(set-frame ...)`
//! that builds a widget tree each frame. The precompute pass calls it,
//! parses the returned [`Value`] into a [`Widget`] tree, and the renderer
//! walks the tree into ratatui draws.
//!
//! This replaces the old `Region` + `RegionAnchor` + `Producer` machinery:
//! status segments, top/bottom strips, gutters, and overlay placement all
//! collapse into stack/line/block widgets.
//!
//! Widgets the lisp side can construct:
//!
//! | Lisp                                 | Variant          |
//! |--------------------------------------|------------------|
//! | `(text "hi" 'face)`                  | `Line`           |
//! | `(line [span1 span2 ...])`           | `Line`           |
//! | `(vstack [...])` / `(hstack [...])`  | `Stack`          |
//! | `(cells N child)` / `(min-cells N c)`| `Constrained`    |
//! | `(fill N child)` / `(frac p q child)`| `Constrained`    |
//! | `(block child {...})`                | `Block`          |
//! | `(editor-tree {...})`                | `EditorTree`     |
//! | `(minibuffer)`                       | `Minibuffer`     |
//! | `(buffer-view)` / `(buffer-view N)`  | `BufferView`     |
//! | `()` / unrecognized                  | `Empty`          |

use std::rc::Rc;

use ratatui::layout::{Alignment, Constraint};
use ratatui::text::Span;
use rizz::runtime::{RuntimeError, Value};

use crate::popup::BorderStyle;
use crate::styling::{Theme, spans_from_value};

#[derive(Clone, Debug)]
pub enum Widget {
    /// Nothing — empty area.
    Empty,
    /// One screen row of styled spans, with horizontal alignment within the
    /// allocated rect.
    Line {
        spans: Vec<Span<'static>>,
        align: Alignment,
    },
    /// Vertical or horizontal stack of children. Each child may be wrapped in
    /// `Constrained` to give it an explicit ratatui constraint; raw children
    /// default to `Min(1)`.
    Stack {
        dir: StackDir,
        children: Vec<Widget>,
    },
    /// Wraps a child with an explicit constraint when it appears inside a
    /// [`Widget::Stack`]. Outside a stack the constraint is ignored.
    Constrained {
        kind: ConstraintKind,
        n: u16,
        m: u16,
        child: Box<Widget>,
    },
    /// Bordered/titled box around a child. Face references resolve through
    /// the active theme at draw time.
    Block {
        border: BorderStyle,
        title: Option<Rc<str>>,
        face: Option<Rc<str>>,
        border_face: Option<Rc<str>>,
        title_face: Option<Rc<str>>,
        child: Box<Widget>,
    },
    /// The editor window tree leaf. Renderer expands this into the current
    /// `WindowTree`'s split layout and draws each leaf as a buffer view.
    /// Optional gutter is rendered to the left of each buffer's content.
    EditorTree {
        gutter_width: u16,
        gutter: Option<Rc<Value>>,
    },
    /// The minibuffer leaf. Single row.
    Minibuffer,
    /// Render a single buffer into the allocated rect via `EditorView`. When
    /// `bufno` is `None`, the renderer fills it in with the enclosing popup's
    /// backing buffer — that's what lets a popup widget declare
    /// `(buffer-view)` without knowing its own bufno yet.
    BufferView { bufno: Option<usize> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackDir {
    Vertical,
    Horizontal,
}

/// Ratatui constraint kinds exposed to lisp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConstraintKind {
    /// Exact cell length.
    Cells,
    /// Minimum cell length; fills remaining space.
    Min,
    /// Proportional fill weight.
    Fill,
    /// Ratio `n/m` of the available extent.
    Frac,
}

impl Widget {
    /// Translate this widget's outer constraint (if any) into a ratatui
    /// [`Constraint`]. Used by stack children that wrap themselves in
    /// `(cells N ...)` / `(min-cells N ...)` etc.
    pub fn outer_constraint(&self) -> Constraint {
        if let Widget::Constrained { kind, n, m, .. } = self {
            match kind {
                ConstraintKind::Cells => Constraint::Length(*n),
                ConstraintKind::Min => Constraint::Min(*n),
                ConstraintKind::Fill => Constraint::Fill(*n),
                ConstraintKind::Frac => Constraint::Ratio(*n as u32, (*m).max(1) as u32),
            }
        } else {
            Constraint::Min(1)
        }
    }

    /// If this is a `Constrained` wrapper, peel it off and return the inner
    /// widget; otherwise return self. Used by the renderer after it has
    /// consumed the wrapper's constraint.
    pub fn unwrap_constraint(&self) -> &Widget {
        match self {
            Widget::Constrained { child, .. } => child,
            _ => self,
        }
    }
}

// ---------------------------------------------------------------------------
// Parsing: Value -> Widget
// ---------------------------------------------------------------------------

/// Parse a widget [`Value`] (the shape returned by `vstack`, `text`, etc.)
/// into a [`Widget`]. `()` and any value missing a recognized `"type"` tag
/// become `Widget::Empty` so a partially-broken layout still renders something.
pub fn parse_widget(v: &Rc<Value>, theme: &Theme) -> Result<Widget, RuntimeError> {
    match &**v {
        Value::Unit => Ok(Widget::Empty),
        Value::Str(_) | Value::Ident(_) | Value::Int(_) => Ok(Widget::Line {
            spans: spans_from_value(v, theme)?,
            align: Alignment::Left,
        }),
        Value::Array(_) | Value::Cons { .. } => {
            // A bare array/list of spans is treated as a single line.
            Ok(Widget::Line {
                spans: spans_from_value(v, theme)?,
                align: Alignment::Left,
            })
        }
        Value::Map(m) => {
            let ty = m
                .get(&key("type"))
                .and_then(|t| t.as_str())
                .unwrap_or_default();
            match ty.as_ref() {
                "line" => parse_line(m, theme),
                "stack" => parse_stack(m, theme),
                "constrained" => parse_constrained(m, theme),
                "block" => parse_block(m, theme),
                "editor-tree" => parse_editor_tree(m),
                "minibuffer" => Ok(Widget::Minibuffer),
                "buffer-view" => parse_buffer_view(m),
                "empty" => Ok(Widget::Empty),
                // A bare span map ({text, style}) — render as a single-line.
                _ if m.contains_key(&key("text")) => Ok(Widget::Line {
                    spans: spans_from_value(v, theme)?,
                    align: Alignment::Left,
                }),
                _ => Ok(Widget::Empty),
            }
        }
        _ => Ok(Widget::Empty),
    }
}

fn parse_line(
    m: &im::HashMap<Rc<Value>, Rc<Value>>,
    theme: &Theme,
) -> Result<Widget, RuntimeError> {
    let spans = match m.get(&key("spans")) {
        Some(v) => spans_from_value(v, theme)?,
        None => Vec::new(),
    };
    let align = m
        .get(&key("align"))
        .and_then(|v| v.as_str())
        .as_deref()
        .map(parse_alignment)
        .unwrap_or(Alignment::Left);
    Ok(Widget::Line { spans, align })
}

fn parse_alignment(s: &str) -> Alignment {
    match s {
        "right" => Alignment::Right,
        "center" => Alignment::Center,
        _ => Alignment::Left,
    }
}

fn parse_stack(
    m: &im::HashMap<Rc<Value>, Rc<Value>>,
    theme: &Theme,
) -> Result<Widget, RuntimeError> {
    let dir = match m
        .get(&key("dir"))
        .and_then(|v| v.as_str())
        .as_deref()
        .unwrap_or("vertical")
    {
        "horizontal" => StackDir::Horizontal,
        _ => StackDir::Vertical,
    };
    let mut children = Vec::new();
    if let Some(cs) = m.get(&key("children")) {
        for c in value_iter(cs) {
            children.push(parse_widget(&c, theme)?);
        }
    }
    Ok(Widget::Stack { dir, children })
}

fn parse_constrained(
    m: &im::HashMap<Rc<Value>, Rc<Value>>,
    theme: &Theme,
) -> Result<Widget, RuntimeError> {
    let kind = match m
        .get(&key("kind"))
        .and_then(|v| v.as_str())
        .as_deref()
        .unwrap_or("min")
    {
        "cells" => ConstraintKind::Cells,
        "min" => ConstraintKind::Min,
        "fill" => ConstraintKind::Fill,
        "frac" => ConstraintKind::Frac,
        _ => ConstraintKind::Min,
    };
    let n = m
        .get(&key("n"))
        .and_then(|v| v.as_int())
        .map(|n| n.max(0).min(u16::MAX as i64) as u16)
        .unwrap_or(1);
    let denom = m
        .get(&key("m"))
        .and_then(|v| v.as_int())
        .map(|m| m.max(1).min(u16::MAX as i64) as u16)
        .unwrap_or(1);
    let child = match m.get(&key("child")) {
        Some(c) => Box::new(parse_widget(c, theme)?),
        None => Box::new(Widget::Empty),
    };
    Ok(Widget::Constrained {
        kind,
        n,
        m: denom,
        child,
    })
}

fn parse_block(
    m: &im::HashMap<Rc<Value>, Rc<Value>>,
    theme: &Theme,
) -> Result<Widget, RuntimeError> {
    let border = m
        .get(&key("border"))
        .and_then(|v| v.as_str())
        .as_deref()
        .map(parse_border)
        .unwrap_or(BorderStyle::None);
    let title = m
        .get(&key("title"))
        .and_then(|v| v.as_str())
        .map(|s| s.clone());
    let face = ident_or_str(m.get(&key("face")));
    let border_face = ident_or_str(m.get(&key("border-face")));
    let title_face = ident_or_str(m.get(&key("title-face")));
    let child = match m.get(&key("child")) {
        Some(c) => Box::new(parse_widget(c, theme)?),
        None => Box::new(Widget::Empty),
    };
    Ok(Widget::Block {
        border,
        title,
        face,
        border_face,
        title_face,
        child,
    })
}

fn parse_buffer_view(m: &im::HashMap<Rc<Value>, Rc<Value>>) -> Result<Widget, RuntimeError> {
    let bufno = m
        .get(&key("bufno"))
        .and_then(|v| v.as_int())
        .map(|n| n.max(0) as usize);
    Ok(Widget::BufferView { bufno })
}

fn parse_editor_tree(m: &im::HashMap<Rc<Value>, Rc<Value>>) -> Result<Widget, RuntimeError> {
    let gutter_width = m
        .get(&key("gutter-width"))
        .and_then(|v| v.as_int())
        .map(|n| n.max(0).min(u16::MAX as i64) as u16)
        .unwrap_or(0);
    let gutter = m.get(&key("gutter")).cloned();
    let gutter = gutter.filter(|v| !v.is_unit());
    Ok(Widget::EditorTree {
        gutter_width,
        gutter,
    })
}

pub fn parse_border(s: &str) -> BorderStyle {
    match s {
        "none" => BorderStyle::None,
        "rounded" => BorderStyle::Rounded,
        "double" => BorderStyle::Double,
        "thick" => BorderStyle::Thick,
        _ => BorderStyle::Plain,
    }
}

fn ident_or_str(v: Option<&Rc<Value>>) -> Option<Rc<str>> {
    v.and_then(|x| match &**x {
        Value::Ident(s) | Value::Str(s) => Some(s.clone()),
        _ => None,
    })
}

fn key(s: &str) -> Rc<Value> {
    Rc::new(Value::Str(s.into()))
}

fn value_iter(v: &Rc<Value>) -> Box<dyn Iterator<Item = Rc<Value>> + '_> {
    match &**v {
        Value::Array(xs) => Box::new(xs.iter().cloned().collect::<Vec<_>>().into_iter()),
        _ => Box::new(Value::iter(v)),
    }
}
