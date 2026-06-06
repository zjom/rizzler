use std::io::{self, Stdout};

use crossterm::{cursor::SetCursorStyle, execute};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

use crate::{
    components::{EditorView, MinibufferLine},
    mode::EditingMode,
    popup::{BorderStyle, Popup},
    render::{CursorStyle, RenderedFrame, Renderer, StateSnapshot},
    styling::{Style, Theme, style_to_ratatui},
    widget::{StackDir, Widget},
};

/// Per-walk context. `popup` is `Some` when the walk is inside a popup's
/// widget tree — that's what resolves `Widget::BufferView { None }` to the
/// enclosing popup's backing buffer and what lets the walker place the
/// cursor inside the popup when it's on top.
#[derive(Clone, Copy, Default)]
struct WalkCtx {
    popup: Option<PopupCtx>,
}

#[derive(Clone, Copy)]
struct PopupCtx {
    bufno: usize,
    is_top: bool,
    show_cursor: bool,
}

/// Concrete ratatui renderer. Walks the widget tree produced by the
/// precompute pass into ratatui draws. Stateless wrt customization — every
/// widget tree was assembled in lisp; this renderer just translates it.
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

/// Cursor placement collected by the walker so it can be applied after the
/// frame is fully drawn. Editor windows publish theirs to `editor`; popups
/// take precedence if their `show_cursor` flag is set.
#[derive(Default)]
struct CursorPlacement {
    editor: Option<(u16, u16)>,
    popup: Option<(u16, u16, CursorStyle)>,
}

impl Renderer for RatatuiRenderer {
    fn render(&mut self, snap: StateSnapshot<'_>, frame_data: &RenderedFrame) -> io::Result<()> {
        execute!(io::stdout(), set_cursor_style(snap.cursor_style))?;

        self.term.draw(|f| {
            // Base fill: paint the whole frame with the `default` face so any
            // cell not overridden by a more specific widget inherits the
            // editor's background and foreground.
            let base_style = style_to_ratatui(&frame_data.default_style);
            f.render_widget(Block::default().style(base_style), f.area());

            let mut cur = CursorPlacement::default();
            walk(
                &frame_data.root,
                f.area(),
                &snap,
                frame_data,
                f,
                &mut cur,
                WalkCtx::default(),
            );

            // Popups draw last, bottom-up so the last entry ends up on top.
            for (i, popup) in snap.popups.iter().enumerate() {
                let is_top = i + 1 == snap.popups.len();
                draw_popup(popup, f.area(), &snap, frame_data, is_top, f, &mut cur);
            }

            if let Some((x, y, cs)) = cur.popup {
                let _ = execute!(io::stdout(), set_cursor_style(cs));
                f.set_cursor_position((x, y));
            } else if snap.popups.is_empty()
                && let Some((x, y)) = cur.editor
            {
                f.set_cursor_position((x, y));
            }
        })?;
        Ok(())
    }
}

/// Recursive widget walker. Lays each widget out in `area` and renders to
/// the ratatui frame. Editor windows update `cur.editor` so the renderer
/// can place the cursor after the frame is drawn. `ctx` propagates the
/// enclosing popup info so `BufferView` leaves resolve to the right buffer
/// and can publish their cursor.
#[allow(clippy::too_many_arguments)]
fn walk(
    w: &Widget,
    area: Rect,
    snap: &StateSnapshot<'_>,
    fd: &RenderedFrame,
    f: &mut Frame,
    cur: &mut CursorPlacement,
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
            walk_stack(*dir, children, area, snap, fd, f, cur, ctx)
        }
        Widget::Constrained { child, .. } => walk(child, area, snap, fd, f, cur, ctx),
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
            ctx,
        ),
        Widget::EditorTree { .. } => walk_editor_tree(area, snap, fd, f, cur),
        Widget::Minibuffer => {
            MinibufferLine::render(area, snap, f);
            if let Some(pos) = MinibufferLine::cursor(area, snap) {
                cur.editor = Some(pos);
            }
        }
        Widget::BufferView { bufno } => {
            walk_buffer_view(*bufno, area, snap, fd, f, cur, ctx);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_stack(
    dir: StackDir,
    children: &[Widget],
    area: Rect,
    snap: &StateSnapshot<'_>,
    fd: &RenderedFrame,
    f: &mut Frame,
    cur: &mut CursorPlacement,
    ctx: WalkCtx,
) {
    if children.is_empty() {
        return;
    }
    let constraints: Vec<Constraint> = children.iter().map(|c| c.outer_constraint()).collect();
    let direction = match dir {
        StackDir::Vertical => Direction::Vertical,
        StackDir::Horizontal => Direction::Horizontal,
    };
    let rects = Layout::default()
        .direction(direction)
        .constraints(constraints)
        .split(area);
    for (child, rect) in children.iter().zip(rects.iter()) {
        walk(child.unwrap_constraint(), *rect, snap, fd, f, cur, ctx);
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_block(
    border: BorderStyle,
    title: Option<&str>,
    face: Option<&str>,
    border_face: Option<&str>,
    title_face: Option<&str>,
    child: &Widget,
    area: Rect,
    snap: &StateSnapshot<'_>,
    fd: &RenderedFrame,
    f: &mut Frame,
    cur: &mut CursorPlacement,
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
    walk(child, inner, snap, fd, f, cur, ctx);
}

#[allow(clippy::too_many_arguments)]
fn walk_buffer_view(
    explicit_bufno: Option<usize>,
    area: Rect,
    snap: &StateSnapshot<'_>,
    fd: &RenderedFrame,
    f: &mut Frame,
    cur: &mut CursorPlacement,
    ctx: WalkCtx,
) {
    let Some(bufno) = explicit_bufno.or(ctx.popup.map(|p| p.bufno)) else {
        return;
    };
    let Some(buf) = snap.bufs.get(bufno) else {
        return;
    };
    let buf_frame = fd.per_buf.get(bufno);
    EditorView::render(buf, area, buf_frame, f);

    // Publish the cursor when this buffer view sits inside the topmost
    // popup and that popup wants its cursor visible.
    if let Some(pctx) = ctx.popup
        && pctx.is_top
        && pctx.show_cursor
        && pctx.bufno == bufno
    {
        let (x, y) = match buf_frame.and_then(|bf| bf.wrap.as_ref()) {
            Some(wrap) => EditorView::cursor_wrapped(buf, area, buf_frame, wrap),
            None => EditorView::cursor(buf, area, buf_frame),
        };
        let cs = match buf.mode() {
            EditingMode::Insert | EditingMode::Command => CursorStyle::Bar,
            _ => CursorStyle::Block,
        };
        cur.popup = Some((x, y, cs));
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
    for leaf in snap.windows.layout(area) {
        let Some(buf) = snap.bufs.get(leaf.bufno) else {
            continue;
        };
        let buf_frame = fd.per_buf.get(leaf.bufno);
        EditorView::render(buf, leaf.area, buf_frame, f);
        if !snap.focus_minibuffer && &leaf.path == focused_path {
            cur.editor = Some(match buf_frame.and_then(|bf| bf.wrap.as_ref()) {
                Some(wrap) => EditorView::cursor_wrapped(buf, leaf.area, buf_frame, wrap),
                None => EditorView::cursor(buf, leaf.area, buf_frame),
            });
        }
    }
}

/// Draw a single popup. Resolves the placement rect, clears it (so the
/// editor underneath stops bleeding through), then walks the popup's
/// widget tree at that rect. Any chrome (block/border/title/face) and the
/// buffer rendering both come from the widget tree — there are no
/// popup-specific draw paths anymore.
fn draw_popup(
    popup: &Popup,
    area: Rect,
    snap: &StateSnapshot<'_>,
    fd: &RenderedFrame,
    is_top: bool,
    f: &mut Frame,
    cur: &mut CursorPlacement,
) {
    let outer = popup.placement.resolve(area);
    if outer.width == 0 || outer.height == 0 {
        return;
    }
    f.render_widget(Clear, outer);
    let ctx = WalkCtx {
        popup: Some(PopupCtx {
            bufno: popup.bufno,
            is_top,
            show_cursor: popup.show_cursor,
        }),
    };
    walk(&popup.widget, outer, snap, fd, f, cur, ctx);
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
