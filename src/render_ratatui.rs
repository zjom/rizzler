use std::io::{self, Stdout};

use crossterm::{cursor::SetCursorStyle, execute};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

use crate::{
    components::{EditorView, MinibufferLine, StatusLine},
    mode::EditingMode,
    popup::{BorderStyle, Popup},
    render::{CursorStyle, RenderedFrame, Renderer, StateSnapshot},
    styling::{Style, Theme, style_to_ratatui},
};

/// Concrete ratatui renderer. Stateless wrt customization — every gutter,
/// segment, decorator, and bottom row is fed by `RenderedFrame`. The
/// renderer just lays out rectangles and copies pre-styled spans onto the
/// terminal.
///
/// Layout, top to bottom: editor area (window tree) → status line → any
/// user-added bottom rows in order → minibuffer.
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

impl Renderer for RatatuiRenderer {
    fn render(&mut self, snap: StateSnapshot<'_>, frame_data: &RenderedFrame) -> io::Result<()> {
        // Cursor style is a terminal escape, not a ratatui widget — emit it
        // out-of-band before the frame draw.
        let style = match snap.cursor_style {
            CursorStyle::Bar => SetCursorStyle::SteadyBar,
            CursorStyle::Block => SetCursorStyle::SteadyBlock,
        };
        execute!(io::stdout(), style)?;

        self.term.draw(|f| {
            // Base layer: paint the whole frame with the `default` face's
            // style so any cell not overridden by a more specific span
            // inherits the editor background and foreground. ratatui's
            // `Paragraph` patches cell styles, so spans with `bg: None` (the
            // common case) preserve this fill.
            let base_style = style_to_ratatui(&frame_data.default_style);
            f.render_widget(Block::default().style(base_style), f.area());

            // Vertical layout: each top strip → editor → status (1) → each
            // bottom strip → minibuffer (1).
            let mut constraints = Vec::new();
            for t in &frame_data.top_extra {
                constraints.push(Constraint::Length(t.lines.len() as u16));
            }
            let editor_idx = constraints.len();
            constraints.push(Constraint::Min(1));
            let status_idx = constraints.len();
            constraints.push(Constraint::Length(1));
            let bottom_start = constraints.len();
            for b in &frame_data.bottom_extra {
                constraints.push(Constraint::Length(b.lines.len() as u16));
            }
            constraints.push(Constraint::Length(1));
            let rects = Layout::default()
                .direction(Direction::Vertical)
                .constraints(constraints)
                .split(f.area());

            for (t, area) in frame_data
                .top_extra
                .iter()
                .zip(rects.iter().take(editor_idx))
            {
                draw_strip(t, *area, f);
            }
            let editor_area = rects[editor_idx];
            let status_area = rects[status_idx];
            let minibuffer_area = *rects.last().unwrap();

            let mut cursor: Option<(u16, u16)> = None;
            let focused_path = snap.windows.focused_path();
            for leaf in snap.windows.layout(editor_area) {
                let Some(buf) = snap.bufs.get(leaf.bufno) else {
                    continue;
                };
                let buf_frame = frame_data.per_buf.get(leaf.bufno);
                EditorView::render(buf, leaf.area, buf_frame, f);
                if !snap.focus_minibuffer && &leaf.path == focused_path {
                    cursor = Some(EditorView::cursor(buf, leaf.area, buf_frame));
                }
            }

            StatusLine::render(
                status_area,
                &frame_data.status_left,
                &frame_data.status_right,
                f,
            );

            // Extra user-added bottom rows occupy rects[2..rects.len()-1].
            for (b, area) in frame_data
                .bottom_extra
                .iter()
                .zip(rects.iter().skip(bottom_start))
            {
                draw_strip(b, *area, f);
            }

            MinibufferLine::render(minibuffer_area, &snap, f);
            if let Some(pos) = MinibufferLine::cursor(minibuffer_area, &snap) {
                cursor = Some(pos);
            }

            // Popups draw last, bottom-up so the last entry ends up on top.
            // While a popup is open the editor cursor is hidden — only the
            // topmost popup may opt into showing its own cursor.
            let mut popup_cursor: Option<(u16, u16, CursorStyle)> = None;
            for (i, popup) in snap.popups.iter().enumerate() {
                let is_top = i + 1 == snap.popups.len();
                let pc = draw_popup(popup, editor_area, &snap, frame_data, is_top, f);
                if is_top {
                    popup_cursor = pc;
                }
            }

            if let Some((x, y, cs)) = popup_cursor {
                // A popup with `show_cursor` overrides the editor cursor and
                // optionally its style — terminal-style popups want a bar.
                let popup_style = match cs {
                    CursorStyle::Bar => SetCursorStyle::SteadyBar,
                    CursorStyle::Block => SetCursorStyle::SteadyBlock,
                };
                let _ = execute!(io::stdout(), popup_style);
                f.set_cursor_position((x, y));
            } else if snap.popups.is_empty()
                && let Some((x, y)) = cursor
            {
                f.set_cursor_position((x, y));
            }
        })?;
        Ok(())
    }
}

/// Draw a single popup. Pipeline:
///
///   1. `Placement::resolve` → outer rect.
///   2. Clear + fill with the popup's background face (or `default`).
///   3. Optional border + title, styled via `border_face` / `title_face`.
///   4. Render the popup's backing buffer via [`EditorView`], reusing the
///      precomputed prop_ranges in `frame_data.per_buf[bufno]`. No gutter
///      because popup buffers skip the region phase.
///
/// Returns the cursor position (if `is_top && popup.show_cursor`).
fn draw_popup(
    popup: &Popup,
    area: Rect,
    snap: &StateSnapshot<'_>,
    frame_data: &RenderedFrame,
    is_top: bool,
    f: &mut ratatui::Frame,
) -> Option<(u16, u16, CursorStyle)> {
    let outer = popup.placement.resolve(area);
    if outer.width == 0 || outer.height == 0 {
        return None;
    }
    f.render_widget(Clear, outer);

    let bg_style = resolve_face(&frame_data.theme, popup.chrome.face.as_deref())
        .unwrap_or_else(|| frame_data.default_style.clone());
    let border_style = resolve_face(&frame_data.theme, popup.chrome.border_face.as_deref())
        .unwrap_or_else(|| bg_style.clone());
    let title_style = resolve_face(&frame_data.theme, popup.chrome.title_face.as_deref())
        .unwrap_or_else(|| border_style.clone());

    let mut block = Block::default().style(style_to_ratatui(&bg_style));
    if popup.chrome.border != BorderStyle::None {
        block = block
            .borders(Borders::ALL)
            .border_type(border_type(popup.chrome.border))
            .border_style(style_to_ratatui(&border_style));
    }
    if let Some(title) = &popup.chrome.title {
        block = block.title(Span::styled(
            format!(" {title} "),
            style_to_ratatui(&title_style),
        ));
    }
    let inner = block.inner(outer);
    f.render_widget(block, outer);

    let Some(buf) = snap.bufs.get(popup.bufno) else {
        return None;
    };
    let buf_frame = frame_data.per_buf.get(popup.bufno);
    EditorView::render(buf, inner, buf_frame, f);

    if is_top && popup.show_cursor {
        let (x, y) = EditorView::cursor(buf, inner, buf_frame);
        let cs = match buf.mode() {
            EditingMode::Insert | EditingMode::Command => CursorStyle::Bar,
            _ => CursorStyle::Block,
        };
        Some((x, y, cs))
    } else {
        None
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

fn draw_strip(b: &crate::render::RenderedStrip, area: Rect, f: &mut ratatui::Frame) {
    let rows: Vec<Line<'static>> = b
        .lines
        .iter()
        .take(area.height as usize)
        .map(|spans| Line::from(spans.clone()))
        .collect();
    f.render_widget(Paragraph::new(rows), area);
}
