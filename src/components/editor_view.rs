use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::buffer::Buffer;

/// Left-side column attached to the editor view — line numbers, signs, diff
/// markers, breakpoints, etc. Each gutter renders its own column.
pub trait Gutter {
    /// Width this gutter wants. May depend on buffer state — e.g. line numbers
    /// need more columns as the file grows.
    fn width(&self, buf: &Buffer) -> u16;
    /// Render the entry for one visible row. `lnum` is the absolute file line
    /// index, or `None` for rows past EOF.
    fn render(&self, lnum: Option<usize>, buf: &Buffer) -> Line<'static>;
}

/// Per-line styling pass applied to the editor content area. Decorators run
/// in order; each receives the line built by the previous one, so syntax
/// highlighting can run before a current-line background.
pub trait LineDecorator {
    fn decorate(&self, lnum: usize, line: &mut Line<'static>, area_width: u16, buf: &Buffer);
}

/// Renders one buffer into a rect. Held by the host renderer and invoked
/// once per window leaf rather than once per frame.
pub struct EditorView {
    gutters: Vec<Box<dyn Gutter>>,
    decorators: Vec<Box<dyn LineDecorator>>,
}

impl EditorView {
    pub fn new(gutters: Vec<Box<dyn Gutter>>, decorators: Vec<Box<dyn LineDecorator>>) -> Self {
        Self {
            gutters,
            decorators,
        }
    }

    /// Width occupied by gutters — used by the host renderer to translate a
    /// buffer cursor position into screen coordinates.
    pub fn gutter_width(&self, buf: &Buffer) -> u16 {
        self.gutters.iter().map(|g| g.width(buf)).sum()
    }

    pub fn render(&self, buf: &Buffer, area: Rect, frame: &mut Frame) {
        // Horizontal split: each gutter takes its requested width, content gets the rest.
        let mut constraints: Vec<Constraint> = self
            .gutters
            .iter()
            .map(|g| Constraint::Length(g.width(buf)))
            .collect();
        constraints.push(Constraint::Min(1));
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(constraints)
            .split(area);
        let content_area = cols[cols.len() - 1];

        let start = buf.file_pos().row.min(buf.len_lines());
        let visible_rows = content_area.height as usize;
        let last_line = buf.len_lines().saturating_sub(1);

        // --- gutter columns ---
        for (i, gutter) in self.gutters.iter().enumerate() {
            let lines: Vec<Line<'static>> = (0..visible_rows)
                .map(|row| {
                    let lnum = start + row;
                    let lnum = (lnum <= last_line).then_some(lnum);
                    gutter.render(lnum, buf)
                })
                .collect();
            frame.render_widget(Paragraph::new(lines), cols[i]);
        }

        // --- content lines through the decorator chain ---
        let content: Vec<Line<'static>> = buf
            .lines_at(start)
            .take(visible_rows)
            .enumerate()
            .map(|(row, l)| {
                let text = l.to_string();
                let text = text.trim_end_matches(['\n', '\r']).to_string();
                let mut line = Line::from(Span::raw(text));
                let lnum = start + row;
                for d in &self.decorators {
                    d.decorate(lnum, &mut line, content_area.width, buf);
                }
                line
            })
            .collect();
        frame.render_widget(Paragraph::new(content), content_area);
    }

    /// Cursor screen position for `buf` displayed in `area`.
    pub fn cursor(&self, buf: &Buffer, area: Rect) -> (u16, u16) {
        let gutter_w = self.gutter_width(buf);
        let cur = buf.cursor_pos();
        (area.x + gutter_w + cur.col, area.y + cur.row)
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

// --- built-in gutters ---

/// Absolute file line number, right-padded by one space.
pub struct LineNumbers;
impl Gutter for LineNumbers {
    fn width(&self, buf: &Buffer) -> u16 {
        let max = buf.len_lines().max(1);
        let digits = ((max as f64).log10().floor() as u16) + 1;
        digits.max(2) + 1
    }
    fn render(&self, lnum: Option<usize>, buf: &Buffer) -> Line<'static> {
        let w = self.width(buf) as usize - 1;
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
    fn decorate(&self, _lnum: usize, line: &mut Line<'static>, _w: u16, _buf: &Buffer) {
        line.style = line.style.patch(Style::default().fg(self.0));
    }
}

/// Background-highlights the line the cursor is on, padding to area width so
/// the highlight extends across the row.
pub struct CurrentLineHighlight(pub Color);
impl LineDecorator for CurrentLineHighlight {
    fn decorate(&self, lnum: usize, line: &mut Line<'static>, area_width: u16, buf: &Buffer) {
        let cur_row = buf.file_pos().row + buf.cursor_pos().row as usize;
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
