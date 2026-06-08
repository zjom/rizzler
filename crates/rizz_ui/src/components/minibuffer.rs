use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::render::StateSnapshot;

/// Single-row strip at the bottom of the frame. Renders the minibuffer's
/// rope text and owns the cursor whenever focus is on the minibuffer.
pub struct MinibufferLine;

impl MinibufferLine {
    pub fn render(area: Rect, snap: &StateSnapshot<'_>, frame: &mut Frame) {
        let text = snap.minibuffer.text();
        let line = Line::from(Span::raw(text));
        frame.render_widget(Paragraph::new(line), area);
    }

    pub fn cursor(area: Rect, snap: &StateSnapshot<'_>) -> Option<(u16, u16)> {
        if !snap.focus_minibuffer() {
            return None;
        }
        let cur = snap.minibuffer.cursor_pos();
        Some((area.x + cur.col, area.y + cur.row))
    }
}
