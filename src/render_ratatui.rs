use std::io::{self, Stdout};

use crossterm::{cursor::SetCursorStyle, execute};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::{
    components::{EditorView, MinibufferLine, StatusLine},
    render::{CursorStyle, RenderedFrame, Renderer, StateSnapshot},
    state::MessagePopup,
    styling::style_to_ratatui,
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

            // Popup is drawn last so it covers every layer below it. While
            // it's visible the editor cursor is hidden — ratatui only shows
            // a cursor when `set_cursor_position` is called this frame.
            if let Some(popup) = snap.message_popup {
                draw_popup(popup, editor_area, f);
            } else if let Some((x, y)) = cursor {
                f.set_cursor_position((x, y));
            }
        })?;
        Ok(())
    }
}

/// Center a `[60% × 60%]` (capped) popup over `area`, paint a bordered box,
/// and draw the popup's wrapped text scrolled by `popup.scroll`.
fn draw_popup(popup: &MessagePopup, area: Rect, f: &mut ratatui::Frame) {
    let w = area.width.saturating_sub(4).clamp(20, 80);
    let h = area.height.saturating_sub(4).clamp(5, 20);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let rect = Rect::new(x, y, w, h);

    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" message — any key to dismiss ");
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let lines: Vec<Line<'static>> = popup
        .text
        .lines()
        .map(|l| Line::from(l.to_string()))
        .collect();
    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((popup.scroll, 0));
    f.render_widget(para, inner);
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
