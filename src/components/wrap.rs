//! Visual-line layout for soft-wrapped buffers.
//!
//! `WrapMap` is a per-render lookup table that maps file rows to one-or-more
//! *visual rows* (what the user sees on screen). It is built fresh each render
//! by the precompute pass for buffers that have wrapping enabled, then handed
//! to `EditorView` for both content emission and cursor translation.
//!
//! Source of truth for the cursor stays in file coordinates (`Buffer::abs_pos`).
//! `WrapMap::locate` derives the screen coordinate from that — the two can
//! never drift, which is the bug the ratatui `Paragraph::wrap` path has.

use crate::buffer::Buffer;
pub use crate::wrap::{WrapConfig, WrapMode};

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
    /// Build a map covering file rows `[start_row, start_row + max_visual_rows)`
    /// at most. Stops early once `max_visual_rows` visual rows are emitted,
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
                        // Trim trailing newline from the visible slice.
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

            // Leading indent for breakindent: count whitespace chars at start.
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
                    break; // pathological: indent ≥ width, give up to avoid infinite loop
                }
                let hard_end = (col + avail).min(n);
                let break_at = match cfg.mode {
                    WrapMode::Word if hard_end < n => {
                        // Search backwards from hard_end for whitespace.
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
    /// `visual_row_index` is relative to `self.rows[0]` — the caller adds the
    /// area's `y` to get the terminal row. `screen_col` already includes any
    /// continuation indent.
    ///
    /// Returns `None` if the position falls outside the rows the map covers
    /// — that means the buffer needs to scroll before the cursor can be
    /// drawn. The view path should handle that by extending the map or
    /// adjusting `file_pos`.
    pub fn locate(&self, file_row: usize, file_col: usize) -> Option<(usize, u16)> {
        // Linear scan by file_row is fine — the visible map is bounded by
        // viewport height. Binary search only matters at much larger sizes.
        let mut idx = self
            .rows
            .iter()
            .position(|r| r.file_row == file_row && file_col < r.end_col)?;
        // Cursor *at* end_col of a non-final segment belongs to the next
        // segment's start. The check above handles `<`; the special "cursor
        // past last char" case lands here:
        if self.rows[idx].end_col == file_col {
            if let Some(next) = self.rows.get(idx + 1)
                && next.file_row == file_row
            {
                idx += 1;
            }
        }
        let row = &self.rows[idx];
        let col = (file_col - row.start_col) as u16 + row.indent;
        Some((idx, col))
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
        // No trailing \n so ropey doesn't add an empty final line.
        let b = buf_from("abcdefghij\nshort");
        let cfg = WrapConfig {
            mode: WrapMode::Char,
            width: 4,
            breakindent: false,
        };
        let map = WrapMap::build(&b, 0, 10, cfg);
        // Row 0: "abcd" / "efgh" / "ij"; row 1: "short".
        assert_eq!(map.rows.len(), 5);
        assert_eq!((map.rows[0].file_row, map.rows[0].start_col, map.rows[0].end_col), (0, 0, 4));
        assert_eq!((map.rows[2].file_row, map.rows[2].start_col, map.rows[2].end_col), (0, 8, 10));
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
        // "hello " (6) fits, "world" doesn't — break after "hello ".
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
        // Char 5 ('f') sits at visual row 1, col 1.
        assert_eq!(map.locate(0, 5), Some((1, 1)));
        // Char 0 ('a') sits at visual row 0, col 0.
        assert_eq!(map.locate(0, 0), Some((0, 0)));
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
        // Continuation rows carry the 4-space indent.
        assert!(map.rows[1].indent >= 4);
    }
}
