use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::Component;
use crate::mode::EditingMode;
use crate::render::StateSnapshot;

/// One piece of the status line — mode glyph, filename, cursor position, etc.
/// Segments produce a `Span` so each can carry its own style.
pub trait Segment {
    fn render(&self, snap: &StateSnapshot<'_>) -> Span<'static>;
}

pub struct StatusLine {
    left: Vec<Box<dyn Segment>>,
    right: Vec<Box<dyn Segment>>,
}

impl StatusLine {
    pub fn new(left: Vec<Box<dyn Segment>>, right: Vec<Box<dyn Segment>>) -> Self {
        Self { left, right }
    }
}

impl Default for StatusLine {
    fn default() -> Self {
        Self::new(
            vec![Box::new(ModeGlyph), Box::new(CommandBuf)],
            vec![Box::new(LastKey), Box::new(Spacer(2)), Box::new(BufferNo)],
        )
    }
}

impl Component for StatusLine {
    fn constraint(&self) -> Constraint {
        Constraint::Length(1)
    }

    fn render(&self, area: Rect, snap: &StateSnapshot<'_>, frame: &mut Frame) {
        let left: Vec<Span<'static>> = self.left.iter().map(|s| s.render(snap)).collect();
        let right: Vec<Span<'static>> = self.right.iter().map(|s| s.render(snap)).collect();
        let right_width: u16 = right
            .iter()
            .map(|s| s.content.chars().count() as u16)
            .sum();
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0), Constraint::Length(right_width)])
            .split(area);
        frame.render_widget(Paragraph::new(Line::from(left)), cols[0]);
        frame.render_widget(Paragraph::new(Line::from(right)), cols[1]);
    }
}

// --- built-in segments ---

pub struct ModeGlyph;
impl Segment for ModeGlyph {
    fn render(&self, snap: &StateSnapshot<'_>) -> Span<'static> {
        Span::raw(match snap.mode {
            EditingMode::Insert => "i",
            EditingMode::Normal => "n",
            EditingMode::Visual => "v",
            EditingMode::Command => ":",
        })
    }
}

pub struct CommandBuf;
impl Segment for CommandBuf {
    fn render(&self, snap: &StateSnapshot<'_>) -> Span<'static> {
        Span::raw(snap.command_buf.to_string())
    }
}

pub struct LastKey;
impl Segment for LastKey {
    fn render(&self, snap: &StateSnapshot<'_>) -> Span<'static> {
        Span::raw(
            snap.keyevent
                .map(|e| e.code.to_string())
                .unwrap_or_else(|| "None".to_string()),
        )
    }
}

pub struct BufferNo;
impl Segment for BufferNo {
    fn render(&self, snap: &StateSnapshot<'_>) -> Span<'static> {
        Span::raw(snap.bufno.to_string())
    }
}

pub struct Spacer(pub usize);
impl Segment for Spacer {
    fn render(&self, _snap: &StateSnapshot<'_>) -> Span<'static> {
        Span::raw(" ".repeat(self.0))
    }
}
