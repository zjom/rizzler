use std::io::{self, Stdout};

use crossterm::{cursor::SetCursorStyle, execute};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    text::Line,
    widgets::Paragraph,
};

use crate::{
    components::{EditorView, MinibufferLine, StatusLine},
    render::{CursorStyle, RenderedFrame, Renderer, StateSnapshot},
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
            // Vertical layout: editor, status (1), each extra bottom row,
            // minibuffer (1).
            let mut constraints = vec![Constraint::Min(1), Constraint::Length(1)];
            for b in &frame_data.bottom_extra {
                constraints.push(Constraint::Length(b.lines.len() as u16));
            }
            constraints.push(Constraint::Length(1));
            let rects = Layout::default()
                .direction(Direction::Vertical)
                .constraints(constraints)
                .split(f.area());

            let editor_area = rects[0];
            let status_area = rects[1];
            let minibuffer_area = *rects.last().unwrap();

            let mut cursor: Option<(u16, u16)> = None;
            let focused_path = snap.windows.focused_path();
            for leaf in snap.windows.layout(editor_area) {
                let Some(buf) = snap.bufs.get(leaf.bufno) else { continue };
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
                .zip(rects.iter().skip(2).take(frame_data.bottom_extra.len()))
            {
                draw_extra_bottom(b, *area, f);
            }

            MinibufferLine::render(minibuffer_area, &snap, f);
            if let Some(pos) = MinibufferLine::cursor(minibuffer_area, &snap) {
                cursor = Some(pos);
            }

            if let Some((x, y)) = cursor {
                f.set_cursor_position((x, y));
            }
        })?;
        Ok(())
    }
}

fn draw_extra_bottom(b: &crate::render::RenderedBottom, area: Rect, f: &mut ratatui::Frame) {
    let rows: Vec<Line<'static>> = b
        .lines
        .iter()
        .take(area.height as usize)
        .map(|spans| Line::from(spans.clone()))
        .collect();
    f.render_widget(Paragraph::new(rows), area);
}
