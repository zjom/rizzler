//! Soft-wrap configuration + visual-line layout.
//!
//! `WrapMap` is a lookup table that maps file rows to one-or-more *visual
//! rows* (what the user sees on screen). It's built by the render precompute
//! pass for buffers with wrapping enabled, cached on `Buffer`, and consumed
//! by both the renderer (to emit visual rows) and cursor movement (to step
//! by visual rows rather than file rows).
//!
//! The cursor's source of truth stays in file coordinates
//! (`Buffer::abs_pos`). `WrapMap::locate` derives screen coordinates from
//! that — the two can't drift, which is the bug ratatui's `Paragraph::wrap`
//! introduces.

use crate::buffer::Buffer;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WrapMode {
    /// No wrapping. `WrapMap` is not built; render walks rope lines directly.
    #[default]
    None,
    /// Break at the column limit, mid-word if necessary.
    Char,
    /// Break at the last whitespace before the column limit. Falls back to
    /// `Char` for tokens longer than the wrap width.
    Word,
}

impl WrapMode {
    pub fn as_str(self) -> &'static str {
        match self {
            WrapMode::None => "none",
            WrapMode::Char => "char",
            WrapMode::Word => "word",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "none" | "off" => WrapMode::None,
            "char" => WrapMode::Char,
            "word" => WrapMode::Word,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct WrapConfig {
    pub mode: WrapMode,
    /// Wrap width in cells. The caller passes the content area width
    /// (post-gutter) — `WrapMap` doesn't know about gutters.
    pub width: u16,
    /// If true, continuation rows are prefixed with leading whitespace
    /// matching the original line's indent.
    pub breakindent: bool,
}

/// One visual row. `[start_col, end_col)` are character offsets within
/// `file_row` (not byte offsets — ropey indexes by char).
#[derive(Debug, Clone, Copy)]
pub struct VisualRow {
    pub file_row: usize,
    pub start_col: usize,
    pub end_col: usize,
    /// Indent (cells) inserted before the slice when this is a continuation
    /// row and `breakindent` is on. 0 for the first segment of a file row.
    pub indent: u16,
}

#[derive(Debug, Default, Clone)]
pub struct WrapMap {
    /// Visual rows in display order, starting at `start_file_row` of the
    /// build call. Length is bounded by the caller's row budget.
    pub rows: Vec<VisualRow>,
    /// File row the first entry of `rows` belongs to. Used to translate
    /// absolute file rows into indices without scanning.
    pub start_file_row: usize,
}

impl WrapMap {
    /// Build a map starting at `start_row` and producing at most
    /// `max_visual_rows` visual rows. Stops early once the budget is hit
    /// since anything past that is scrolled out anyway.
    pub fn build(
        buf: &Buffer,
        start_row: usize,
        max_visual_rows: usize,
        cfg: WrapConfig,
    ) -> Self {
        let mut rows = Vec::with_capacity(max_visual_rows);
        if cfg.width == 0 || matches!(cfg.mode, WrapMode::None) {
            // Degenerate: one visual row per file row, no wrapping.
            for r in start_row..(start_row + max_visual_rows).min(buf.len_lines()) {
                let line = buf.lines_at(r).next();
                let len = line
                    .map(|l| {
                        let n = l.len_chars();
                        if n > 0 && l.char(n - 1) == '\n' {
                            n - 1
                        } else {
                            n
                        }
                    })
                    .unwrap_or(0);
                rows.push(VisualRow {
                    file_row: r,
                    start_col: 0,
                    end_col: len,
                    indent: 0,
                });
            }
            return Self {
                rows,
                start_file_row: start_row,
            };
        }

        let last_line = buf.len_lines();
        let mut file_row = start_row;
        while file_row < last_line && rows.len() < max_visual_rows {
            let line = match buf.lines_at(file_row).next() {
                Some(l) => l,
                None => break,
            };
            let mut n = line.len_chars();
            if n > 0 && line.char(n - 1) == '\n' {
                n -= 1;
            }

            let indent = if cfg.breakindent {
                let mut i = 0;
                while i < n
                    && let Some(c) = line.get_char(i)
                    && (c == ' ' || c == '\t')
                {
                    i += 1;
                }
                (i as u16).min(cfg.width.saturating_sub(1))
            } else {
                0
            };

            if n == 0 {
                rows.push(VisualRow {
                    file_row,
                    start_col: 0,
                    end_col: 0,
                    indent: 0,
                });
                file_row += 1;
                continue;
            }

            let mut col = 0usize;
            let mut is_continuation = false;
            while col < n && rows.len() < max_visual_rows {
                let cont_indent = if is_continuation { indent } else { 0 };
                let avail = cfg.width.saturating_sub(cont_indent) as usize;
                if avail == 0 {
                    break;
                }
                let hard_end = (col + avail).min(n);
                let break_at = match cfg.mode {
                    WrapMode::Word if hard_end < n => {
                        let mut k = hard_end;
                        while k > col && !is_break_char(line.char(k - 1)) {
                            k -= 1;
                        }
                        if k == col { hard_end } else { k }
                    }
                    _ => hard_end,
                };
                rows.push(VisualRow {
                    file_row,
                    start_col: col,
                    end_col: break_at,
                    indent: cont_indent,
                });
                col = break_at;
                is_continuation = true;
            }
            file_row += 1;
        }

        Self {
            rows,
            start_file_row: start_row,
        }
    }

    /// Resolve an absolute file position to `(visual_row_index, screen_col)`.
    /// `visual_row_index` is the offset into `self.rows`. `screen_col`
    /// already includes any continuation indent.
    ///
    /// Returns `None` if the position falls outside the rows the map
    /// covers — the caller should rebuild the map or scroll.
    pub fn locate(&self, file_row: usize, file_col: usize) -> Option<(usize, u16)> {
        // Find the segment containing file_col on the matching file row.
        // The cursor at end_col of a non-final segment of the same file row
        // belongs to the next segment's start, not the previous segment's
        // end — match that boundary case explicitly.
        let mut idx = self.rows.iter().position(|r| {
            r.file_row == file_row && r.start_col <= file_col && file_col <= r.end_col
        })?;
        if self.rows[idx].end_col == file_col
            && let Some(next) = self.rows.get(idx + 1)
            && next.file_row == file_row
        {
            idx += 1;
        }
        let row = &self.rows[idx];
        let col = (file_col - row.start_col) as u16 + row.indent;
        Some((idx, col))
    }

    /// Inverse of `locate`: given a visual row index and a *visual* column
    /// (post-indent), return the absolute file (row, col). Visual cols past
    /// the segment's end are clamped to the segment's last column.
    pub fn file_pos_at(&self, visual_idx: usize, visual_col: u16) -> Option<(usize, usize)> {
        let row = self.rows.get(visual_idx)?;
        let post_indent = visual_col.saturating_sub(row.indent) as usize;
        let max_inside = row.end_col.saturating_sub(row.start_col);
        let inside = post_indent.min(max_inside);
        Some((row.file_row, row.start_col + inside))
    }

    /// Index of the visual row that begins `file_row` (its first segment).
    /// Used by scroll math to align the viewport top to a file row.
    pub fn first_visual_idx_of(&self, file_row: usize) -> Option<usize> {
        self.rows.iter().position(|r| r.file_row == file_row)
    }
}

fn is_break_char(c: char) -> bool {
    c == ' ' || c == '\t' || c == '-'
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    fn buf_from(text: &str) -> Buffer {
        let mut b = Buffer::new();
        b.buf = Rope::from_str(text);
        b
    }

    #[test]
    fn char_mode_splits_long_line() {
        let b = buf_from("abcdefghij\nshort");
        let cfg = WrapConfig {
            mode: WrapMode::Char,
            width: 4,
            breakindent: false,
        };
        let map = WrapMap::build(&b, 0, 10, cfg);
        assert_eq!(map.rows.len(), 5);
        assert_eq!(
            (map.rows[0].file_row, map.rows[0].start_col, map.rows[0].end_col),
            (0, 0, 4)
        );
        assert_eq!(
            (map.rows[2].file_row, map.rows[2].start_col, map.rows[2].end_col),
            (0, 8, 10)
        );
        assert_eq!(map.rows[3].file_row, 1);
    }

    #[test]
    fn word_mode_breaks_at_whitespace() {
        let b = buf_from("hello world wide");
        let cfg = WrapConfig {
            mode: WrapMode::Word,
            width: 8,
            breakindent: false,
        };
        let map = WrapMap::build(&b, 0, 10, cfg);
        assert_eq!(map.rows[0].end_col, 6);
    }

    #[test]
    fn locate_translates_cursor_to_visual_coords() {
        let b = buf_from("abcdefghij");
        let cfg = WrapConfig {
            mode: WrapMode::Char,
            width: 4,
            breakindent: false,
        };
        let map = WrapMap::build(&b, 0, 10, cfg);
        assert_eq!(map.locate(0, 5), Some((1, 1)));
        assert_eq!(map.locate(0, 0), Some((0, 0)));
        // Cursor at boundary between segments — belongs to next.
        assert_eq!(map.locate(0, 4), Some((1, 0)));
        // Cursor at end of last segment of the file row.
        assert_eq!(map.locate(0, 10), Some((2, 2)));
    }

    #[test]
    fn file_pos_at_round_trips_with_locate() {
        let b = buf_from("abcdefghij");
        let cfg = WrapConfig {
            mode: WrapMode::Char,
            width: 4,
            breakindent: false,
        };
        let map = WrapMap::build(&b, 0, 10, cfg);
        for col in 0..=10 {
            let (vi, vc) = map.locate(0, col).unwrap();
            let (fr, fc) = map.file_pos_at(vi, vc).unwrap();
            assert_eq!((fr, fc), (0, col), "round trip failed for col {col}");
        }
    }

    #[test]
    fn breakindent_offsets_continuation_rows() {
        let b = buf_from("    long line of text here");
        let cfg = WrapConfig {
            mode: WrapMode::Char,
            width: 10,
            breakindent: true,
        };
        let map = WrapMap::build(&b, 0, 10, cfg);
        assert_eq!(map.rows[0].indent, 0);
        assert!(map.rows[1].indent >= 4);
    }
}
