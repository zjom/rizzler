//! Frame layout as a widget tree. The user's `init.lisp` registers a
//! function via `(set-frame ...)` that builds the tree each frame; the
//! precompute pass parses its return value into [`Widget`], and the
//! renderer walks it into ratatui draws.

use std::rc::Rc;

use ratatui::layout::{Alignment, Constraint};
use ratatui::text::Span;
use rizz::runtime::{RuntimeError, Value};
use rizz_text::BufferId;
use slotmap::KeyData;

use crate::panel::{BorderStyle, Placement, parse_placement};
use crate::styling::{Theme, spans_from_value};

#[derive(Clone, Debug)]
pub enum Widget {
    Empty,
    /// One screen row of styled spans with horizontal alignment.
    Line {
        spans: Vec<Span<'static>>,
        align: Alignment,
    },
    Stack {
        dir: StackDir,
        children: Vec<Widget>,
    },
    /// Wraps a child with an explicit constraint when inside a
    /// [`Widget::Stack`]; ignored elsewhere.
    Constrained {
        kind: ConstraintKind,
        n: u16,
        m: u16,
        child: Box<Widget>,
    },
    Block {
        border: BorderStyle,
        title: Option<Rc<str>>,
        face: Option<Rc<str>>,
        border_face: Option<Rc<str>>,
        title_face: Option<Rc<str>>,
        child: Box<Widget>,
    },
    /// Editor split layout. Gutter content + width are state-level (see
    /// `(set-gutter fn width)`), so they aren't encoded here.
    EditorTree,
    Minibuffer,
    /// Render a single buffer. When `buf` is `None`, the renderer uses the
    /// enclosing popup's backing buffer.
    BufferView {
        buf: Option<BufferId>,
    },
    /// Non-focusable floating overlay — paints `child` over the rest of
    /// the frame in a post-pass. Has no backing buffer and never receives
    /// keys.
    Overlay {
        placement: Placement,
        child: Box<Widget>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackDir {
    Vertical,
    Horizontal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConstraintKind {
    Cells,
    Min,
    Fill,
    Frac,
}

impl Widget {
    /// Translate the outer constraint into a ratatui [`Constraint`].
    /// [`Widget::Empty`] takes zero cells so `(w-empty)` truly disappears
    /// from a stack when its predicate is false.
    pub fn outer_constraint(&self) -> Constraint {
        match self {
            Widget::Constrained { kind, n, m, .. } => match kind {
                ConstraintKind::Cells => Constraint::Length(*n),
                ConstraintKind::Min => Constraint::Min(*n),
                ConstraintKind::Fill => Constraint::Fill(*n),
                ConstraintKind::Frac => Constraint::Ratio(*n as u32, (*m).max(1) as u32),
            },
            Widget::Empty => Constraint::Length(0),
            _ => Constraint::Min(1),
        }
    }

    pub fn unwrap_constraint(&self) -> &Widget {
        match self {
            Widget::Constrained { child, .. } => child,
            _ => self,
        }
    }

    /// Direct subwidgets — walkers should prefer this so new variants
    /// only require one match arm here.
    pub fn children(&self) -> WidgetChildren<'_> {
        match self {
            Widget::Stack { children, .. } => WidgetChildren::Many(children.iter()),
            Widget::Constrained { child, .. }
            | Widget::Block { child, .. }
            | Widget::Overlay { child, .. } => WidgetChildren::One(Some(child)),
            _ => WidgetChildren::Empty,
        }
    }
}

/// Collect every explicitly-targeted buffer id (`(w-buffer-view N)`)
/// reachable from `w`. Used by the precompute pass to find buffers that are
/// visible without being a window leaf or a panel's backing buffer.
pub fn collect_buffer_views(w: &Widget, out: &mut Vec<BufferId>) {
    if let Widget::BufferView { buf: Some(id) } = w {
        out.push(*id);
    }
    for c in w.children() {
        collect_buffer_views(c, out);
    }
}

/// Allocation-free iterator over a widget's direct children.
pub enum WidgetChildren<'a> {
    Empty,
    One(Option<&'a Widget>),
    Many(std::slice::Iter<'a, Widget>),
}

impl<'a> Iterator for WidgetChildren<'a> {
    type Item = &'a Widget;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            WidgetChildren::Empty => None,
            WidgetChildren::One(slot) => slot.take(),
            WidgetChildren::Many(it) => it.next(),
        }
    }
}

/// Parse a widget [`Value`] (the shape returned by `vstack`, `text`, etc.).
/// `()` and any value missing a recognized `"type"` tag become
/// [`Widget::Empty`] so a partially broken layout still renders.
pub fn parse_widget(v: &Rc<Value>, theme: &Theme) -> Result<Widget, RuntimeError> {
    match &**v {
        Value::Unit => Ok(Widget::Empty),
        Value::Str(_) | Value::Ident(_) | Value::Int(_) => Ok(Widget::Line {
            spans: spans_from_value(v, theme)?,
            align: Alignment::Left,
        }),
        Value::Array(_) | Value::Cons { .. } => Ok(Widget::Line {
            spans: spans_from_value(v, theme)?,
            align: Alignment::Left,
        }),
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
                "editor-tree" => Ok(Widget::EditorTree),
                "minibuffer" => Ok(Widget::Minibuffer),
                "buffer-view" => parse_buffer_view(m),
                "overlay" => parse_overlay(m, theme),
                "empty" => Ok(Widget::Empty),
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
    let title = m.get(&key("title")).and_then(|v| v.as_str());
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

fn parse_overlay(
    m: &im::HashMap<Rc<Value>, Rc<Value>>,
    theme: &Theme,
) -> Result<Widget, RuntimeError> {
    let placement = match m.get(&key("placement")) {
        Some(v) => parse_placement(v)?,
        None => Placement::default(),
    };
    let child = match m.get(&key("child")) {
        Some(c) => Box::new(parse_widget(c, theme)?),
        None => Box::new(Widget::Empty),
    };
    Ok(Widget::Overlay { placement, child })
}

fn parse_buffer_view(m: &im::HashMap<Rc<Value>, Rc<Value>>) -> Result<Widget, RuntimeError> {
    let buf = m
        .get(&key("bufno"))
        .and_then(|v| v.as_int())
        .filter(|&n| n > 0)
        .map(|n| BufferId::from(KeyData::from_ffi(n as u64)));
    Ok(Widget::BufferView { buf })
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
