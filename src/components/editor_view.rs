use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::Component;
use crate::render::StateSnapshot;

/// Left-side column attached to the editor view — line numbers, signs, diff
/// markers, breakpoints, etc. Each gutter renders its own column.
pub trait Gutter {
    /// Width this gutter wants. May depend on buffer state — e.g. line numbers
    /// need more columns as the file grows.
    fn width(&self, snap: &StateSnapshot<'_>) -> u16;
    /// Render the entry for one visible row. `lnum` is the absolute file line
    /// index, or `None` for rows past EOF.
    fn render(&self, lnum: Option<usize>, snap: &StateSnapshot<'_>) -> Line<'static>;
}

/// Per-line styling pass applied to the editor content area. Decorators run
/// in order; each receives the line built by the previous one, so syntax
/// highlighting can run before a current-line background.
pub trait LineDecorator {
    fn decorate(
        &self,
        lnum: usize,
        line: &mut Line<'static>,
        area_width: u16,
        snap: &StateSnapshot<'_>,
    );
}

pub struct EditorView {
    gutters: Vec<Box<dyn Gutter>>,
    decorators: Vec<Box<dyn LineDecorator>>,
}

impl EditorView {
    pub fn new(gutters: Vec<Box<dyn Gutter>>, decorators: Vec<Box<dyn LineDecorator>>) -> Self {
        Self { gutters, decorators }
    }
}

impl Default for EditorView {
    fn default() -> Self {
        Self::new(
            vec![Box::new(LineNumbers)],
            vec![
                Box::new(BaseFg(Color::Blue)),
                Box::new(CurrentLineHighlight(Color::DarkGray)),
            ],
        )
    }
}

impl Component for EditorView {
    fn constraint(&self) -> Constraint {
        Constraint::Min(1)
    }

    fn render(&self, area: Rect, snap: &StateSnapshot<'_>, frame: &mut Frame) {
        // Horizontal split: each gutter takes its requested width, content gets the rest.
        let mut constraints: Vec<Constraint> = self
            .gutters
            .iter()
            .map(|g| Constraint::Length(g.width(snap)))
            .collect();
        constraints.push(Constraint::Min(1));
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(constraints)
            .split(area);
        let content_area = cols[cols.len() - 1];

        let start = snap.buffer.file_pos().row.min(snap.buffer.len_lines());
        let visible_rows = content_area.height as usize;
        let last_line = snap.buffer.len_lines().saturating_sub(1);

        // --- gutter columns ---
        for (i, gutter) in self.gutters.iter().enumerate() {
            let lines: Vec<Line<'static>> = (0..visible_rows)
                .map(|row| {
                    let lnum = start + row;
                    let lnum = (lnum <= last_line).then_some(lnum);
                    gutter.render(lnum, snap)
                })
                .collect();
            frame.render_widget(Paragraph::new(lines), cols[i]);
        }

        // --- content lines through the decorator chain ---
        let content: Vec<Line<'static>> = snap
            .buffer
            .lines_at(start)
            .take(visible_rows)
            .enumerate()
            .map(|(row, l)| {
                let text = l.to_string();
                let text = text.trim_end_matches(['\n', '\r']).to_string();
                let mut line = Line::from(Span::raw(text));
                let lnum = start + row;
                for d in &self.decorators {
                    d.decorate(lnum, &mut line, content_area.width, snap);
                }
                line
            })
            .collect();
        frame.render_widget(Paragraph::new(content), content_area);
    }

    fn cursor(&self, area: Rect, snap: &StateSnapshot<'_>) -> Option<(u16, u16)> {
        let gutter_w: u16 = self.gutters.iter().map(|g| g.width(snap)).sum();
        let cur = snap.buffer.cursor_pos();
        Some((area.x + gutter_w + cur.col, area.y + cur.row))
    }
}

// --- built-in gutters ---

/// Absolute file line number, right-padded by one space.
pub struct LineNumbers;
impl Gutter for LineNumbers {
    fn width(&self, snap: &StateSnapshot<'_>) -> u16 {
        let max = snap.buffer.len_lines().max(1);
        let digits = ((max as f64).log10().floor() as u16) + 1;
        digits.max(2) + 1
    }
    fn render(&self, lnum: Option<usize>, snap: &StateSnapshot<'_>) -> Line<'static> {
        let w = self.width(snap) as usize - 1;
        match lnum {
            Some(n) => Line::from(Span::raw(format!("{:>w$} ", n, w = w))),
            None => Line::from(Span::raw(" ".repeat(w + 1))),
        }
    }
}

// --- built-in decorators ---

/// Sets the foreground color of every line. Cheap base coat.
pub struct BaseFg(pub Color);
impl LineDecorator for BaseFg {
    fn decorate(&self, _lnum: usize, line: &mut Line<'static>, _w: u16, _snap: &StateSnapshot<'_>) {
        line.style = line.style.patch(Style::default().fg(self.0));
    }
}

/// Background-highlights the line the cursor is on, padding to area width so
/// the highlight extends across the row.
pub struct CurrentLineHighlight(pub Color);
impl LineDecorator for CurrentLineHighlight {
    fn decorate(
        &self,
        lnum: usize,
        line: &mut Line<'static>,
        area_width: u16,
        snap: &StateSnapshot<'_>,
    ) {
        let cur_row = snap.buffer.file_pos().row + snap.buffer.cursor_pos().row as usize;
        if lnum != cur_row {
            return;
        }
        line.style = line.style.patch(Style::default().bg(self.0));
        let used: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
        if (area_width as usize) > used {
            line.spans
                .push(Span::raw(" ".repeat(area_width as usize - used)));
        }
    }
}
