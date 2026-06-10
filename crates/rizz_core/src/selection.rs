//! Visual-selection extraction. Pure functions over rope + cursor + anchor;
//! buffers store the anchor and delegate per-mode slicing here.

use ropey::Rope;

use crate::mode::EditingMode;
use crate::position::FilePos;

/// Text covered by the current visual selection. Returns `None` when `mode`
/// is non-visual. Inclusive on both ends; `VisualLine` includes the trailing
/// newline of the last selected row, and `VisualBlock` joins each row's
/// column slice with `\n`.
pub fn selected_text(
    rope: &Rope,
    mode: EditingMode,
    anchor: FilePos,
    cursor: FilePos,
) -> Option<String> {
    match mode {
        EditingMode::Visual => Some(visual_text(rope, anchor, cursor)),
        EditingMode::VisualLine => Some(visual_line_text(rope, anchor, cursor)),
        EditingMode::VisualBlock => Some(visual_block_text(rope, anchor, cursor)),
        _ => None,
    }
}

fn visual_text(rope: &Rope, anchor: FilePos, cursor: FilePos) -> String {
    let (start, end) = if (anchor.row, anchor.col) <= (cursor.row, cursor.col) {
        (anchor, cursor)
    } else {
        (cursor, anchor)
    };
    let s = rope.line_to_char(start.row) + start.col;
    let e = (rope.line_to_char(end.row) + end.col + 1).min(rope.len_chars());
    rope.slice(s..e).to_string()
}

fn visual_line_text(rope: &Rope, anchor: FilePos, cursor: FilePos) -> String {
    let (lo, hi) = if anchor.row <= cursor.row {
        (anchor.row, cursor.row)
    } else {
        (cursor.row, anchor.row)
    };
    let s = rope.line_to_char(lo);
    let last_line = rope.len_lines().saturating_sub(1);
    let e = if hi >= last_line {
        rope.len_chars()
    } else {
        rope.line_to_char(hi + 1)
    };
    rope.slice(s..e).to_string()
}

fn visual_block_text(rope: &Rope, anchor: FilePos, cursor: FilePos) -> String {
    let (lo_row, hi_row) = if anchor.row <= cursor.row {
        (anchor.row, cursor.row)
    } else {
        (cursor.row, anchor.row)
    };
    let (lo_col, hi_col) = if anchor.col <= cursor.col {
        (anchor.col, cursor.col)
    } else {
        (cursor.col, anchor.col)
    };
    let mut out = String::new();
    for row in lo_row..=hi_row {
        let line = rope.line(row);
        let mut len = line.len_chars();
        if len > 0 && line.char(len - 1) == '\n' {
            len -= 1;
        }
        let s = lo_col.min(len);
        let e = (hi_col + 1).min(len);
        if s < e {
            out.push_str(&line.slice(s..e).to_string());
        }
        if row != hi_row {
            out.push('\n');
        }
    }
    out
}
