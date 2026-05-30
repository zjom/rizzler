use std::io::{self, Stdout};

use crossterm::{cursor::SetCursorStyle, execute};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::{
    mode::EditingMode,
    render::{CursorStyle, Renderer, StateSnapshot},
};

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
    fn render(&mut self, snap: StateSnapshot<'_>) -> io::Result<()> {
        // Cursor style is a terminal escape, not a ratatui widget — emit
        // it out-of-band before the frame draw.
        let style = match snap.cursor_style {
            CursorStyle::Bar => SetCursorStyle::SteadyBar,
            CursorStyle::Block => SetCursorStyle::SteadyBlock,
        };
        execute!(io::stdout(), style)?;

        self.term.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .split(f.area());
            let editor_area = chunks[0];
            let status_area = chunks[1];

            // --- editor ---
            let start = snap.buffer.file_pos().row.min(snap.buffer.len_lines());
            let lines: Vec<Line> = snap
                .buffer
                .lines_at(start)
                .take(editor_area.height as usize)
                .map(|l| {
                    let s = l.to_string();
                    Line::from(Span::styled(
                        s.trim_end_matches(['\n', '\r']).to_string(),
                        Style::default().fg(Color::Blue),
                    ))
                })
                .collect();
            f.render_widget(Paragraph::new(lines), editor_area);

            // --- status line ---
            let mode_glyph = match snap.mode {
                EditingMode::Insert => "i",
                EditingMode::Normal => "n",
                EditingMode::Visual => "v",
                EditingMode::Command => ":",
            };
            let left = format!("{}{}", mode_glyph, snap.command_buf);
            let right = format!(
                "{}  {}",
                snap.keyevent
                    .map(|e| e.code.to_string())
                    .unwrap_or_else(|| "None".to_string()),
                snap.bufno,
            );

            let right_width = right.chars().count() as u16;
            let status = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(0), Constraint::Length(right_width)])
                .split(status_area);
            f.render_widget(Paragraph::new(left), status[0]);
            f.render_widget(Paragraph::new(right), status[1]);

            // --- cursor ---
            let cur = snap.buffer.cursor_pos();
            f.set_cursor_position((editor_area.x + cur.col, editor_area.y + cur.row));
        })?;
        Ok(())
    }
}
