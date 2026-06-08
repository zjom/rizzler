//! Concrete ratatui renderer. Walks the widget tree produced by the
//! precompute pass into ratatui draws. Stateless wrt customization — every
//! widget tree was assembled in lisp; this renderer just translates it.

use std::io::{self, Stdout};

use crossterm::{cursor::SetCursorStyle, execute};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

use rizz_core::EditingMode;
use rizz_text::BufferId;

use crate::{
    components::{EditorView, MinibufferLine},
    panel::{BorderStyle, Panel, Placement},
    render::{CursorStyle, RenderedFrame, Renderer, StateSnapshot},
    styling::{Style, Theme, style_to_ratatui},
    widget::{StackDir, Widget},
};

/// Per-walk context. `overlay` is `Some` when the walk is inside an overlay
/// panel's widget tree — that's what resolves `Widget::BufferView { None }`
/// to the enclosing panel's backing buffer and what lets the walker place
/// the cursor inside the panel when it's on top.
#[derive(Clone, Copy, Default)]
struct WalkCtx {
    overlay: Option<OverlayCtx>,
}

#[derive(Clone, Copy)]
struct OverlayCtx {
    buf: BufferId,
    is_top: bool,
    show_cursor: bool,
}

pub struct RatatuiRenderer {
    term: Terminal<CrosstermBackend<Stdout>>,
}

impl RatatuiRenderer {
    pub fn new() -> io::Result<Self> {
        let backend = CrosstermBackend::new(io::stdout());
        Ok(Self {
            term: Terminal::new(backend)?,
        })
    }
}

#[derive(Default)]
struct CursorPlacement {
    editor: Option<(u16, u16)>,
    overlay: Option<(u16, u16, CursorStyle)>,
}

/// One non-focusable `Widget::Overlay` encountered during the main walk,
/// stashed so it can be painted in a post-pass after the rest of the frame.
struct DeferredOverlay<'a> {
    placement: Placement,
    child: &'a Widget,
    area: Rect,
}

impl Renderer for RatatuiRenderer {
    fn render(&mut self, snap: StateSnapshot<'_>, frame_data: &RenderedFrame) -> io::Result<()> {
        execute!(io::stdout(), set_cursor_style(snap.cursor_style))?;

        self.term.draw(|f| {
            let base_style = style_to_ratatui(&frame_data.default_style);
            f.render_widget(Block::default().style(base_style), f.area());

            let mut cur = CursorPlacement::default();
            let mut overlays: Vec<DeferredOverlay<'_>> = Vec::new();
            walk(
                &frame_data.root,
                f.area(),
                &snap,
                frame_data,
                f,
                &mut cur,
                &mut overlays,
                WalkCtx::default(),
            );

            for o in overlays {
                let rect = o.placement.resolve(o.area, 0, 0);
                if rect.width == 0 || rect.height == 0 {
                    continue;
                }
                f.render_widget(Clear, rect);
                let mut sink: Vec<DeferredOverlay<'_>> = Vec::new();
                walk(
                    o.child,
                    rect,
                    &snap,
                    frame_data,
                    f,
                    &mut cur,
                    &mut sink,
                    WalkCtx::default(),
                );
            }

            let panel_overlays: Vec<&Panel> = snap.panels.overlays().collect();
            let last = panel_overlays.len().saturating_sub(1);
            for (i, panel) in panel_overlays.iter().enumerate() {
                let is_top = i == last;
                draw_overlay(panel, f.area(), &snap, frame_data, is_top, f, &mut cur);
            }

            if let Some((x, y, cs)) = cur.overlay {
                let _ = execute!(io::stdout(), set_cursor_style(cs));
                f.set_cursor_position((x, y));
            } else if !snap.panels.any_overlay()
                && let Some((x, y)) = cur.editor
            {
                f.set_cursor_position((x, y));
            }
        })?;
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn walk<'fr>(
    w: &'fr Widget,
    area: Rect,
    snap: &StateSnapshot<'_>,
    fd: &'fr RenderedFrame,
    f: &mut Frame,
    cur: &mut CursorPlacement,
    overlays: &mut Vec<DeferredOverlay<'fr>>,
    ctx: WalkCtx,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    match w {
        Widget::Empty => {}
        Widget::Line { spans, align } => {
            f.render_widget(
                Paragraph::new(Line::from(spans.clone())).alignment(*align),
                area,
            );
        }
        Widget::Stack { dir, children } => {
            walk_stack(*dir, children, area, snap, fd, f, cur, overlays, ctx)
        }
        Widget::Constrained { child, .. } => walk(child, area, snap, fd, f, cur, overlays, ctx),
        Widget::Block {
            border,
            title,
            face,
            border_face,
            title_face,
            child,
        } => walk_block(
            *border,
            title.as_deref(),
            face.as_deref(),
            border_face.as_deref(),
            title_face.as_deref(),
            child,
            area,
            snap,
            fd,
            f,
            cur,
            overlays,
            ctx,
        ),
        Widget::EditorTree => walk_editor_tree(area, snap, fd, f, cur),
        Widget::Minibuffer => {
            MinibufferLine::render(area, snap, f);
            if let Some(pos) = MinibufferLine::cursor(area, snap) {
                cur.editor = Some(pos);
            }
        }
        Widget::BufferView { buf } => {
            walk_buffer_view(*buf, area, snap, fd, f, cur, ctx);
        }
        Widget::Overlay { placement, child } => {
            overlays.push(DeferredOverlay {
                placement: placement.clone(),
                child,
                area,
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_stack<'fr>(
    dir: StackDir,
    children: &'fr [Widget],
    area: Rect,
    snap: &StateSnapshot<'_>,
    fd: &'fr RenderedFrame,
    f: &mut Frame,
    cur: &mut CursorPlacement,
    overlays: &mut Vec<DeferredOverlay<'fr>>,
    ctx: WalkCtx,
) {
    if children.is_empty() {
        return;
    }
    // Overlay children are out-of-flow: they don't take a layout slice from
    // the stack, they float over the stack's full area. Partition them out
    // before computing the layout so the remaining children share the area
    // as if the overlay wasn't there at all.
    let (flow, floats): (Vec<&Widget>, Vec<&Widget>) = children
        .iter()
        .partition(|c| !matches!(c.unwrap_constraint(), Widget::Overlay { .. }));

    if !flow.is_empty() {
        let constraints: Vec<Constraint> = flow.iter().map(|c| c.outer_constraint()).collect();
        let direction = match dir {
            StackDir::Vertical => Direction::Vertical,
            StackDir::Horizontal => Direction::Horizontal,
        };
        let rects = Layout::default()
            .direction(direction)
            .constraints(constraints)
            .split(area);
        for (child, rect) in flow.iter().zip(rects.iter()) {
            walk(
                child.unwrap_constraint(),
                *rect,
                snap,
                fd,
                f,
                cur,
                overlays,
                ctx,
            );
        }
    }
    for c in floats {
        walk(c.unwrap_constraint(), area, snap, fd, f, cur, overlays, ctx);
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_block<'fr>(
    border: BorderStyle,
    title: Option<&str>,
    face: Option<&str>,
    border_face: Option<&str>,
    title_face: Option<&str>,
    child: &'fr Widget,
    area: Rect,
    snap: &StateSnapshot<'_>,
    fd: &'fr RenderedFrame,
    f: &mut Frame,
    cur: &mut CursorPlacement,
    overlays: &mut Vec<DeferredOverlay<'fr>>,
    ctx: WalkCtx,
) {
    let bg = resolve_face(&fd.theme, face).unwrap_or_else(|| fd.default_style.clone());
    let border_style = resolve_face(&fd.theme, border_face).unwrap_or_else(|| bg.clone());
    let title_style = resolve_face(&fd.theme, title_face).unwrap_or_else(|| border_style.clone());

    let mut block = Block::default().style(style_to_ratatui(&bg));
    if border != BorderStyle::None {
        block = block
            .borders(Borders::ALL)
            .border_type(border_type(border))
            .border_style(style_to_ratatui(&border_style));
    }
    if let Some(t) = title {
        block = block.title(Span::styled(
            format!(" {t} "),
            style_to_ratatui(&title_style),
        ));
    }
    let inner = block.inner(area);
    f.render_widget(block, area);
    walk(child, inner, snap, fd, f, cur, overlays, ctx);
}

#[allow(clippy::too_many_arguments)]
fn walk_buffer_view(
    explicit_buf: Option<BufferId>,
    area: Rect,
    snap: &StateSnapshot<'_>,
    fd: &RenderedFrame,
    f: &mut Frame,
    cur: &mut CursorPlacement,
    ctx: WalkCtx,
) {
    let Some(buf_id) = explicit_buf.or(ctx.overlay.map(|p| p.buf)) else {
        return;
    };
    let Some(buf) = snap.bufs.get(buf_id) else {
        return;
    };
    let buf_frame = fd.per_buf.get(buf_id);
    EditorView::render(buf, area, buf_frame, f);

    if let Some(octx) = ctx.overlay
        && octx.is_top
        && octx.show_cursor
        && octx.buf == buf_id
    {
        let (x, y) = match buf_frame.and_then(|bf| bf.wrap.as_ref()) {
            Some(wrap) => EditorView::cursor_wrapped(buf, area, buf_frame, wrap),
            None => EditorView::cursor(buf, area, buf_frame),
        };
        let cs = match buf.mode() {
            EditingMode::Insert | EditingMode::Command => CursorStyle::Bar,
            _ => CursorStyle::Block,
        };
        cur.overlay = Some((x, y, cs));
    }
}

fn walk_editor_tree(
    area: Rect,
    snap: &StateSnapshot<'_>,
    fd: &RenderedFrame,
    f: &mut Frame,
    cur: &mut CursorPlacement,
) {
    let focused_path = snap.windows.focused_path();
    let editor_focused = snap.panels.is_empty();
    for leaf in snap.windows.layout(area) {
        let Some(buf) = snap.bufs.get(leaf.buf) else {
            continue;
        };
        let buf_frame = fd.per_buf.get(leaf.buf);
        EditorView::render(buf, leaf.area, buf_frame, f);
        if editor_focused && &leaf.path == focused_path {
            cur.editor = Some(match buf_frame.and_then(|bf| bf.wrap.as_ref()) {
                Some(wrap) => EditorView::cursor_wrapped(buf, leaf.area, buf_frame, wrap),
                None => EditorView::cursor(buf, leaf.area, buf_frame),
            });
        }
    }
}

fn draw_overlay<'fr>(
    panel: &'fr Panel,
    area: Rect,
    snap: &StateSnapshot<'_>,
    fd: &'fr RenderedFrame,
    is_top: bool,
    f: &mut Frame,
    cur: &mut CursorPlacement,
) {
    let buf = match snap.bufs.get(panel.buf) {
        Some(b) => b,
        None => return,
    };
    let Some((_, widget, show_cursor)) = panel.as_overlay() else {
        return;
    };
    let outer = crate::panel::resolve_overlay_rect(panel, area, buf);
    if outer.width == 0 || outer.height == 0 {
        return;
    }
    f.render_widget(Clear, outer);
    let ctx = WalkCtx {
        overlay: Some(OverlayCtx {
            buf: panel.buf,
            is_top,
            show_cursor,
        }),
    };
    let mut sink: Vec<DeferredOverlay<'_>> = Vec::new();
    walk(widget, outer, snap, fd, f, cur, &mut sink, ctx);
}

fn set_cursor_style(cs: CursorStyle) -> SetCursorStyle {
    match cs {
        CursorStyle::Bar => SetCursorStyle::SteadyBar,
        CursorStyle::Block => SetCursorStyle::SteadyBlock,
    }
}

fn border_type(b: BorderStyle) -> BorderType {
    match b {
        BorderStyle::None | BorderStyle::Plain => BorderType::Plain,
        BorderStyle::Rounded => BorderType::Rounded,
        BorderStyle::Double => BorderType::Double,
        BorderStyle::Thick => BorderType::Thick,
    }
}

fn resolve_face(theme: &Theme, name: Option<&str>) -> Option<Style> {
    name.and_then(|n| theme.resolve(n))
}
