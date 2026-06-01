use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::buffer::Buffer;
use crate::mode::EditingMode;

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
                // Selection runs before CurrentLineHighlight so the span-level
                // selection bg overrides the line-level current-line bg.
                Box::new(SelectionHighlight(Color::Rgb(60, 90, 130))),
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

/// Background-highlights the visual selection on this line, using the buffer's
/// `selection_anchor` and current cursor. Behaves differently per visual mode:
///
/// * `Visual` — characterwise from the earlier endpoint to the later one,
///   inclusive on both ends. Selection is single-line-bounded by `lnum`.
/// * `VisualLine` — full row width (padded out to `area_width`).
/// * `VisualBlock` — the column range `[min_col, max_col]` on every row in
///   `[min_row, max_row]`.
///
/// No-op when the buffer has no anchor or is not in a visual mode.
pub struct SelectionHighlight(pub Color);
impl LineDecorator for SelectionHighlight {
    fn decorate(&self, lnum: usize, line: &mut Line<'static>, area_width: u16, buf: &Buffer) {
        let Some(anchor) = buf.selection_anchor() else {
            return;
        };
        let cur = buf.abs_pos();
        let mode = buf.mode();
        if !mode.is_visual() {
            return;
        }

        let (min_row, max_row) = order(anchor.row, cur.row);
        if lnum < min_row || lnum > max_row {
            return;
        }

        let line_len: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();

        let (start, end) = match mode {
            EditingMode::VisualLine => (0usize, area_width as usize),
            EditingMode::VisualBlock => {
                let (lo, hi) = order(anchor.col, cur.col);
                (lo, (hi + 1).min(area_width as usize))
            }
            EditingMode::Visual => {
                // Order endpoints in (row, col) lexicographic order.
                let (lo_row, lo_col, hi_row, hi_col) =
                    if (anchor.row, anchor.col) <= (cur.row, cur.col) {
                        (anchor.row, anchor.col, cur.row, cur.col)
                    } else {
                        (cur.row, cur.col, anchor.row, anchor.col)
                    };
                let s = if lnum == lo_row { lo_col } else { 0 };
                let e = if lnum == hi_row {
                    hi_col + 1
                } else {
                    line_len.max(1)
                };
                (s, e)
            }
            _ => return,
        };

        if start >= end {
            return;
        }
        paint_bg_range(line, start, end, self.0, area_width);
    }
}

fn order(a: usize, b: usize) -> (usize, usize) {
    if a <= b { (a, b) } else { (b, a) }
}

/// Repaint `line` so that character indices `start..end` carry background
/// `bg`. Pads with spaces to reach `end` (capped by `area_width`) so an
/// empty/short line still shows a visible selection band.
fn paint_bg_range(line: &mut Line<'static>, start: usize, end: usize, bg: Color, area_width: u16) {
    let inherited = line.style;
    let highlight = inherited.bg(bg);

    let mut text: String = line.spans.iter().flat_map(|s| s.content.chars()).collect();
    let end = end.min(area_width as usize);
    let cur_len = text.chars().count();
    if end > cur_len {
        text.extend(std::iter::repeat_n(' ', end - cur_len));
    }
    let chars: Vec<char> = text.chars().collect();
    let start = start.min(chars.len());
    let split = end.min(chars.len());

    let before: String = chars[..start].iter().collect();
    let middle: String = chars[start..split].iter().collect();
    let after: String = chars[split..].iter().collect();

    let mut spans = Vec::with_capacity(3);
    if !before.is_empty() {
        spans.push(Span::styled(before, inherited));
    }
    if !middle.is_empty() {
        spans.push(Span::styled(middle, highlight));
    }
    if !after.is_empty() {
        spans.push(Span::styled(after, inherited));
    }
    line.spans = spans;
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
