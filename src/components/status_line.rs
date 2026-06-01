//! Bottom-strip status line. Pre-rendered into spans during the precompute
//! pass; this module just lays out the left/right buckets and draws them.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

pub struct StatusLine;

impl StatusLine {
    pub fn render(
        area: Rect,
        left: &[Span<'static>],
        right: &[Span<'static>],
        frame: &mut Frame,
    ) {
        // Use display width (not char count) so CJK / wide-emoji segments
        // don't bleed across the left/right split.
        let right_width: u16 = right
            .iter()
            .map(|s| UnicodeWidthStr::width(s.content.as_ref()) as u16)
            .sum();
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0), Constraint::Length(right_width)])
            .split(area);
        frame.render_widget(Paragraph::new(Line::from(left.to_vec())), cols[0]);
        frame.render_widget(Paragraph::new(Line::from(right.to_vec())), cols[1]);
    }
}
