//! Stateless editor view. Gutter and decorator content lives in the
//! `RenderedBuffer` from the precompute pass; this module is glue between
//! that and ratatui's layout/widget primitives.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use rizz_core::Display;
use rizz_text::{Buffer, WrapMap};

use crate::render::{DecoratorRanges, RenderedBuffer, StyledRange};
use crate::styling::style_to_ratatui;

pub struct EditorView;

impl EditorView {
    /// Gutter width reserved for this buffer — used to translate
    /// buffer-relative cursor cols into screen cols.
    pub fn gutter_width(buf_frame: Option<&RenderedBuffer>) -> u16 {
        buf_frame
            .and_then(|f| f.gutter.as_ref())
            .map(|g| g.width)
            .unwrap_or(0)
    }

    pub fn render(buf: &Buffer, area: Rect, buf_frame: Option<&RenderedBuffer>, frame: &mut Frame) {
        let gutter = buf_frame.and_then(|f| f.gutter.as_ref());
        let decorators = buf_frame.map(|f| f.decorators.as_slice()).unwrap_or(&[]);
        let wrap = buf_frame.and_then(|f| f.wrap.as_ref());

        let gutter_w = gutter.map(|g| g.width).unwrap_or(0);
        let mut constraints: Vec<Constraint> = Vec::new();
        if gutter_w > 0 {
            constraints.push(Constraint::Length(gutter_w));
        }
        constraints.push(Constraint::Min(1));
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(constraints)
            .split(area);
        let content_area = cols[cols.len() - 1];

        if let Some(g) = gutter {
            frame.render_widget(Paragraph::new(g.rows.clone()), cols[0]);
        }

        let visible_rows = content_area.height as usize;
        let content: Vec<Line<'static>> = if let Some(wrap) = wrap {
            wrap.rows
                .iter()
                .take(visible_rows)
                .map(|vr| {
                    let line = match buf.lines_at(vr.file_row).next() {
                        Some(l) => l,
                        None => return Line::from(String::new()),
                    };
                    let total = line.len_chars();
                    let end = vr.end_col.min(total);
                    let segment: String = line.slice(vr.start_col..end).to_string();
                    let segment = segment.trim_end_matches(['\n', '\r']).to_string();
                    apply_decorators_segment(
                        vr.file_row,
                        vr.start_col,
                        vr.indent,
                        segment,
                        decorators,
                        content_area.width,
                    )
                })
                .collect()
        } else {
            let start = buf.file_pos().row.min(buf.len_lines());
            buf.lines_at(start)
                .take(visible_rows)
                .enumerate()
                .map(|(row, l)| {
                    let lnum = start + row;
                    let text = l.to_string();
                    let text = text.trim_end_matches(['\n', '\r']).to_string();
                    apply_decorators(lnum, text, decorators, content_area.width)
                })
                .collect()
        };
        frame.render_widget(Paragraph::new(content), content_area);
    }

    pub fn cursor(buf: &Buffer, area: Rect, buf_frame: Option<&RenderedBuffer>) -> (u16, u16) {
        let gutter_w = Self::gutter_width(buf_frame);
        let cur = buf.cursor_pos();
        (area.x + gutter_w + cur.col, area.y + cur.row)
    }

    /// Cursor for soft-wrapped buffers. Derives screen coords from the
    /// buffer's absolute file position via `WrapMap` so the logical and
    /// visual cursors can't drift.
    pub fn cursor_wrapped(
        buf: &Buffer,
        area: Rect,
        buf_frame: Option<&RenderedBuffer>,
        wrap: &WrapMap,
    ) -> (u16, u16) {
        let gutter_w = Self::gutter_width(buf_frame);
        let abs = buf.abs_pos();
        if let Some((visual_row, screen_col)) = wrap.locate(abs.row, abs.col) {
            return (area.x + gutter_w + screen_col, area.y + visual_row as u16);
        }
        (area.x + gutter_w, area.y)
    }
}

fn apply_decorators(
    lnum: usize,
    text: String,
    decorators: &[DecoratorRanges],
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

fn apply_decorators_segment(
    file_row: usize,
    segment_start: usize,
    indent: u16,
    text: String,
    decorators: &[DecoratorRanges],
    area_width: u16,
) -> Line<'static> {
    let segment_len = text.chars().count();
    let segment_end = segment_start + segment_len;
    let inner_width = area_width.saturating_sub(indent);

    let mut spans = vec![Span::raw(text)];
    for d in decorators {
        for r in &d.ranges {
            if r.row != file_row {
                continue;
            }
            let r_end = r.col + r.len;
            if !r.pad_to_width && (r_end <= segment_start || r.col >= segment_end) {
                continue;
            }
            let clipped_col = r.col.saturating_sub(segment_start);
            let clipped_end = r_end
                .saturating_sub(segment_start)
                .min(segment_len.max(r_end));
            let clipped = StyledRange {
                row: r.row,
                col: clipped_col,
                len: clipped_end.saturating_sub(clipped_col),
                style: r.style.clone(),
                pad_to_width: r.pad_to_width,
                display: r.display.clone(),
            };
            spans = apply_range(spans, &clipped, inner_width);
        }
    }

    if indent > 0 {
        let mut with_indent = Vec::with_capacity(spans.len() + 1);
        with_indent.push(Span::raw(" ".repeat(indent as usize)));
        with_indent.extend(spans);
        Line::from(with_indent)
    } else {
        Line::from(spans)
    }
}

fn apply_range(spans: Vec<Span<'static>>, r: &StyledRange, area_width: u16) -> Vec<Span<'static>> {
    let cur_len: usize = spans.iter().map(|s| s.content.chars().count()).sum();

    let pad_len = if r.pad_to_width {
        (area_width as usize).max(cur_len)
    } else {
        cur_len
    };

    let start = r.col.min(pad_len);
    let raw_end = if r.pad_to_width && r.len == 0 {
        pad_len
    } else {
        (r.col + r.len).min(pad_len)
    };
    let end = raw_end.max(start);

    let new_style = style_to_ratatui(&r.style);

    let mut chunks: Vec<(String, Style)> = spans
        .into_iter()
        .map(|s| (s.content.into_owned(), s.style))
        .collect();
    if pad_len > cur_len {
        chunks.push((" ".repeat(pad_len - cur_len), Style::default()));
    }

    let mut before: Vec<Span<'static>> = Vec::new();
    let mut mid: Vec<Span<'static>> = Vec::new();
    let mut after: Vec<Span<'static>> = Vec::new();
    let mut pos = 0usize;
    for (text, style) in chunks {
        let chars: Vec<char> = text.chars().collect();
        let chunk_end = pos + chars.len();
        let s = start.clamp(pos, chunk_end);
        let e = end.clamp(pos, chunk_end);

        if s > pos {
            let part: String = chars[..s - pos].iter().collect();
            before.push(Span::styled(part, style));
        }
        if e > s {
            let part: String = chars[s - pos..e - pos].iter().collect();
            mid.push(Span::styled(part, style.patch(new_style)));
        }
        if chunk_end > e {
            let part: String = chars[e - pos..].iter().collect();
            after.push(Span::styled(part, style));
        }
        pos = chunk_end;
    }

    let middle: Vec<Span<'static>> = match &r.display {
        Some(Display::String(s)) => {
            let base = mid.first().map(|sp| sp.style).unwrap_or(new_style);
            vec![Span::styled(s.to_string(), base)]
        }
        Some(Display::Space(n)) => {
            let base = mid.first().map(|sp| sp.style).unwrap_or(new_style);
            vec![Span::styled(" ".repeat(*n), base)]
        }
        None => mid,
    };

    let mut out: Vec<Span<'static>> = Vec::with_capacity(before.len() + middle.len() + after.len());
    out.extend(before);
    out.extend(middle);
    out.extend(after);
    if out.is_empty() {
        out.push(Span::raw(String::new()));
    }
    out
}
