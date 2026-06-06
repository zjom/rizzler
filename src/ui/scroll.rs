//! Viewport scrolling math. Pure functions over (viewport, wrap cache,
//! cursor position) → new scroll top / cursor target. Pulled out of
//! [`crate::buffer::Buffer`] so the scrolling rules can be reasoned about
//! without buffer state.
//!
//! The buffer still owns the column clamp and the EOF row cap; everything
//! visual-row aware lives here.

use crate::ui::wrap::WrapMap;

/// Top file row that puts `abs_row` at the vertical middle of a `viewport_h`
/// viewport. Saturates at 0 near the top of the buffer.
pub fn centered_top(viewport_h: u16, abs_row: usize) -> usize {
    let vh = viewport_h as usize;
    if vh == 0 {
        return abs_row;
    }
    abs_row.saturating_sub(vh / 2)
}

/// File `(row, col)` that sits `dy` visual rows away from `(abs_row, abs_col)`
/// in the cached wrap layout. `None` when wrap is off, the cache is empty, or
/// the step would exit the cached range (caller falls back to file-row math).
pub fn visual_step(
    wrap: Option<&WrapMap>,
    abs_row: usize,
    abs_col: usize,
    dy: i16,
) -> Option<(usize, usize)> {
    let wrap = wrap?;
    let total = wrap.rows.len() as isize;
    if total == 0 {
        return None;
    }
    let (cur_idx, cur_vcol) = wrap.locate(abs_row, abs_col)?;
    let target_idx_i = cur_idx as isize + dy as isize;
    if target_idx_i < 0 || target_idx_i >= total {
        return None;
    }
    wrap.file_pos_at(target_idx_i as usize, cur_vcol)
}

/// New `file_pos.row` so `abs_row` stays inside a `viewport_h` viewport.
/// `current_top` is the existing scroll position; `last_line` the buffer's
/// last file row.
///
/// When a wrap cache is present the down-scroll check is visual-row aware:
/// "cursor below viewport" counts wrapped rows from `current_top`. Up-scroll
/// stays file-row based because the cache doesn't cover anything above its
/// start. With wrap off, the standard "pin the last line at the viewport
/// bottom" EOF rule applies.
pub fn clamp_scroll_top(
    viewport_h: u16,
    wrap: Option<&WrapMap>,
    current_top: usize,
    abs_row: usize,
    abs_col: usize,
    last_line: usize,
) -> usize {
    if viewport_h == 0 {
        return current_top;
    }
    let vh = viewport_h as usize;

    let visual_top = wrap.and_then(|w| {
        let (cur_idx, _) = w.locate(abs_row, abs_col)?;
        if cur_idx >= vh {
            let target_top_visual = cur_idx + 1 - vh;
            // The visual row at target_top_visual maps to some file row.
            // Continuation rows (start_col != 0) can't be scroll tops — the
            // next render starts the cache at a file-row boundary — so round
            // up to the next file row.
            let target_file_row = w
                .rows
                .get(target_top_visual)
                .map(|r| {
                    if r.start_col == 0 {
                        r.file_row
                    } else {
                        r.file_row + 1
                    }
                })
                .unwrap_or(abs_row);
            // Never scroll past the cursor's own file row.
            Some(target_file_row.min(abs_row))
        } else {
            Some(current_top)
        }
    });

    let mut top = match visual_top {
        Some(t) => t,
        None => {
            let mut t = current_top;
            if abs_row < t {
                t = abs_row;
            } else if abs_row >= t + vh {
                t = abs_row + 1 - vh;
            }
            // EOF pin only correct when 1 file row = 1 visual row.
            let max_top = (last_line + 1).saturating_sub(vh);
            t.min(max_top)
        }
    };
    // Up-scroll if the cursor would still sit above the new top.
    if abs_row < top {
        top = abs_row;
    }
    top
}

/// Target `(file_row, optional_col_override)` for a half-page step.
/// `direction` is `1` for down, `-1` for up. When wrap is on and the visual
/// step lands inside the cache, the overridden col carries the visual-column
/// snap; otherwise the caller keeps the cursor's existing column.
pub fn half_page_target(
    viewport_h: u16,
    wrap: Option<&WrapMap>,
    abs_row: usize,
    abs_col: usize,
    direction: i16,
) -> (usize, Option<usize>) {
    let vh = viewport_h as usize;
    if vh == 0 {
        return (abs_row, None);
    }
    let half = ((vh / 2).max(1)) as isize * direction as isize;

    let visual = wrap.and_then(|w| {
        let total = w.rows.len() as isize;
        if total == 0 {
            return None;
        }
        let (cur_idx, cur_vcol) = w.locate(abs_row, abs_col)?;
        let target_idx_i = (cur_idx as isize + half).clamp(0, total - 1);
        w.file_pos_at(target_idx_i as usize, cur_vcol)
    });

    match visual {
        Some((r, c)) => (r, Some(c)),
        None => {
            let r = (abs_row as isize).saturating_add(half).max(0) as usize;
            (r, None)
        }
    }
}
