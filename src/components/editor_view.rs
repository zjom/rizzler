//! Stateless editor view. All gutter/decorator content lives in the
//! pre-computed `RenderedBuffer` produced by `state::render`; this module is
//! just glue between that and ratatui's layout/widget primitives.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::buffer::Buffer;
use crate::render::{Display, RenderedBuffer, StyledRange};
use crate::styling::style_to_ratatui;

pub struct EditorView;

impl EditorView {
    /// Total gutter width for the buffer, summed across this frame's
    /// rendered gutters. Used to translate buffer-relative cursor cols
    /// into screen cols.
    pub fn gutter_width(buf_frame: Option<&RenderedBuffer>) -> u16 {
        buf_frame
            .map(|f| f.gutters.iter().map(|g| g.width).sum())
            .unwrap_or(0)
    }

    pub fn render(buf: &Buffer, area: Rect, buf_frame: Option<&RenderedBuffer>, frame: &mut Frame) {
        let gutters = buf_frame.map(|f| f.gutters.as_slice()).unwrap_or(&[]);
        let decorators = buf_frame.map(|f| f.decorators.as_slice()).unwrap_or(&[]);

        // Horizontal split: each gutter takes its registered width, content gets the rest.
        let mut constraints: Vec<Constraint> = gutters
            .iter()
            .map(|g| Constraint::Length(g.width))
            .collect();
        constraints.push(Constraint::Min(1));
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(constraints)
            .split(area);
        let content_area = cols[cols.len() - 1];

        // --- gutter columns ---
        for (i, gutter) in gutters.iter().enumerate() {
            frame.render_widget(Paragraph::new(gutter.rows.clone()), cols[i]);
        }

        // --- content lines, with decorator ranges applied ---
        let start = buf.file_pos().row.min(buf.len_lines());
        let visible_rows = content_area.height as usize;
        let content: Vec<Line<'static>> = buf
            .lines_at(start)
            .take(visible_rows)
            .enumerate()
            .map(|(row, l)| {
                let lnum = start + row;
                let text = l.to_string();
                let text = text.trim_end_matches(['\n', '\r']).to_string();
                apply_decorators(lnum, text, decorators, content_area.width)
            })
            .collect();
        frame.render_widget(Paragraph::new(content), content_area);
    }

    pub fn cursor(buf: &Buffer, area: Rect, buf_frame: Option<&RenderedBuffer>) -> (u16, u16) {
        let gutter_w = Self::gutter_width(buf_frame);
        let cur = buf.cursor_pos();
        (area.x + gutter_w + cur.col, area.y + cur.row)
    }
}

/// Walk every decorator's ranges in order, applying any that hit `lnum` to
/// the line's spans. Later ranges layer over earlier ones, mirroring the
/// previous "decorator chain" semantics.
fn apply_decorators(
    lnum: usize,
    text: String,
    decorators: &[crate::render::DecoratorRanges],
    area_width: u16,
) -> Line<'static> {
    let mut spans = vec![Span::raw(text)];
    for d in decorators {
        for r in &d.ranges {
            if r.row != lnum {
                continue;
            }
            spans = apply_range(spans, r, area_width);
        }
    }
    Line::from(spans)
}

/// Repaint `spans` so that character indices `[r.col, r.col + r.len)` carry
/// `r.style`. Pads with spaces when `pad_to_width` is set so a current-line
/// or visual-line band fills the row width. When `r.display` is set, the
/// `middle` slice is *replaced* with the display content instead of
/// restyled — subsequent ranges that target columns after the replacement
/// may end up slightly mis-aligned, which is a known limitation of doing
/// substitution in a flat span stream.
fn apply_range(spans: Vec<Span<'static>>, r: &StyledRange, area_width: u16) -> Vec<Span<'static>> {
    let mut text: String = spans.iter().flat_map(|s| s.content.chars()).collect();
    let cur_len = text.chars().count();

    let pad_len = if r.pad_to_width {
        (area_width as usize).max(cur_len)
    } else {
        cur_len
    };
    if pad_len > cur_len {
        text.extend(std::iter::repeat_n(' ', pad_len - cur_len));
    }

    let chars: Vec<char> = text.chars().collect();
    let end = if r.pad_to_width && r.len == 0 {
        pad_len
    } else {
        (r.col + r.len).min(chars.len())
    };
    let start = r.col.min(chars.len());
    let end = end.max(start);

    // Preserve any pre-existing styled spans outside the new range. We
    // capture the inherited base style from the first span (the simple
    // common case — non-uniform per-char styling would need a denser model
    // but isn't produced by today's range emitters).
    let inherited = spans.first().map(|s| s.style).unwrap_or_default();
    let highlight = inherited.patch(style_to_ratatui(&r.style));

    let before: String = chars[..start].iter().collect();
    let middle: String = match &r.display {
        Some(Display::String(s)) => s.to_string(),
        Some(Display::Space(n)) => " ".repeat(*n),
        None => chars[start..end].iter().collect(),
    };
    let after: String = chars[end..].iter().collect();

    let mut out = Vec::with_capacity(3);
    if !before.is_empty() {
        out.push(Span::styled(before, inherited));
    }
    if !middle.is_empty() {
        out.push(Span::styled(middle, highlight));
    }
    if !after.is_empty() {
        out.push(Span::styled(after, inherited));
    }
    if out.is_empty() {
        out.push(Span::raw(String::new()));
    }
    out
}
