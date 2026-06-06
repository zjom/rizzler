//! Stateless editor view. All gutter/decorator content lives in the
//! pre-computed `RenderedBuffer` produced by `state::render`; this module is
//! just glue between that and ratatui's layout/widget primitives.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::buffer::Buffer;
use crate::ui::render::{Display, RenderedBuffer, StyledRange};
use crate::ui::styling::style_to_ratatui;
use crate::ui::wrap::WrapMap;

pub struct EditorView;

impl EditorView {
    /// Gutter width reserved for this buffer's view. Used to translate
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

        // Horizontal split: gutter (if any) takes its registered width,
        // content gets the rest.
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

        // --- content lines, with decorator ranges applied ---
        let visible_rows = content_area.height as usize;
        let content: Vec<Line<'static>> = if let Some(wrap) = wrap {
            // Wrapped path: emit one Line per visual row.
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
            // Unwrapped path: one screen row per file row.
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

    /// Cursor placement for buffers with soft-wrap on. Derives the screen
    /// coordinate from the buffer's *absolute file position* via `WrapMap`,
    /// so logical and visual cursors can't drift.
    ///
    /// `wrap` is built by the precompute pass for buffers that opt in; its
    /// first entry corresponds to `buf.file_pos().row`.
    pub fn cursor_wrapped(
        buf: &Buffer,
        area: Rect,
        buf_frame: Option<&RenderedBuffer>,
        wrap: &WrapMap,
    ) -> (u16, u16) {
        let gutter_w = Self::gutter_width(buf_frame);
        let abs = buf.abs_pos();
        // `locate` returns indices relative to wrap.rows[0]. The caller has
        // already arranged for wrap.start_file_row == buf.file_pos().row, so
        // the visual index is the screen row offset from area.y.
        if let Some((visual_row, screen_col)) = wrap.locate(abs.row, abs.col) {
            return (area.x + gutter_w + screen_col, area.y + visual_row as u16);
        }
        // Fallback: cursor is off the rendered map — should not happen if
        // the precompute pass kept file_pos in sync with the cursor, but
        // returning the area origin is safer than panicking.
        (area.x + gutter_w, area.y)
    }
}

/// Walk every decorator's ranges in order, applying any that hit `lnum` to
/// the line's spans. Later ranges layer over earlier ones, mirroring the
/// previous "decorator chain" semantics.
fn apply_decorators(
    lnum: usize,
    text: String,
    decorators: &[crate::ui::render::DecoratorRanges],
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

/// Same as `apply_decorators`, but for one visual segment of a wrapped row.
/// Decorator columns are clipped to `[segment_start, segment_start + len(text))`
/// and shifted into segment-local coordinates before being applied. A
/// `breakindent` of `indent` cells is prepended to continuation segments;
/// it does not affect column math (decorators target file columns, not
/// screen columns).
fn apply_decorators_segment(
    file_row: usize,
    segment_start: usize,
    indent: u16,
    text: String,
    decorators: &[crate::ui::render::DecoratorRanges],
    area_width: u16,
) -> Line<'static> {
    let segment_len = text.chars().count();
    let segment_end = segment_start + segment_len;
    // Width available *for the content slice* — pad_to_width must respect
    // the indent we'll prepend below, otherwise current-line highlights
    // would overflow.
    let inner_width = area_width.saturating_sub(indent);

    let mut spans = vec![Span::raw(text)];
    for d in decorators {
        for r in &d.ranges {
            if r.row != file_row {
                continue;
            }
            // Clip the range to this segment.
            let r_end = r.col + r.len;
            // The range still applies if it overlaps the segment OR if it's
            // a pad_to_width range (which intentionally extends past content).
            if !r.pad_to_width && (r_end <= segment_start || r.col >= segment_end) {
                continue;
            }
            let clipped_col = r.col.saturating_sub(segment_start);
            let clipped_end = r_end
                .saturating_sub(segment_start)
                .min(segment_len.max(r_end));
            let clipped = crate::ui::render::StyledRange {
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

/// Repaint `spans` so that character indices `[r.col, r.col + r.len)` carry
/// `r.style`. Pads with spaces when `pad_to_width` is set so a current-line
/// or visual-line band fills the row width. When `r.display` is set, the
/// `middle` slice is *replaced* with the display content instead of
/// restyled — subsequent ranges that target columns after the replacement
/// may end up slightly mis-aligned, which is a known limitation of doing
/// substitution in a flat span stream.
///
/// Spans outside `[start, end)` keep their existing per-span style; spans
/// inside have their style *patched* with the new range's style. This lets
/// multiple ranges layer on the same row without later passes clobbering
/// styles set by earlier passes (the previous implementation flattened the
/// row to a single inherited base style on every call).
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

    // Flatten input spans + any pad_to_width filler into (text, style) chunks.
    // Padding past existing content uses `Style::default()` since it represents
    // empty area past the buffer content.
    let mut chunks: Vec<(String, Style)> = spans
        .into_iter()
        .map(|s| (s.content.into_owned(), s.style))
        .collect();
    if pad_len > cur_len {
        chunks.push((" ".repeat(pad_len - cur_len), Style::default()));
    }

    // Walk chunks, splitting each at `start` and `end`. Pieces inside
    // [start, end) move to `mid` with their style patched by `new_style`;
    // pieces outside keep their style untouched.
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
