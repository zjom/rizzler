//! Text properties and overlays.
//!
//! A **text property** is a buffer-resident annotation: it covers an absolute
//! range `[start, end)` (row+col positions) and carries a property bag
//! (currently just `face`, but the shape is extensible). Properties don't
//! auto-shift through edits in this version — callers are expected to clear
//! and re-apply when the buffer changes.
//!
//! An **overlay** is the same shape but addressed by a stable handle so lisp
//! can mutate or remove individual entries without re-emitting the whole set.
//! Overlays have a `priority`; among overlapping overlays, higher priority
//! wins.
//!
//! Both kinds get walked once per frame in [`build_prop_ranges`] and emitted
//! as [`crate::render::StyledRange`]s the renderer applies on top of the
//! base content.

use std::rc::Rc;

use rizz::runtime::Value;

use crate::buffer::Buffer;
use crate::position::Position;
use crate::render::{DecoratorRanges, Display, StyledRange};
use crate::styling::{Theme, style_from_value};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// An absolute half-open range `[start, end)` with a property bag. Used for
/// both text properties (anonymous, batch-applied) and overlays (named via
/// `id`, individually mutable).
#[derive(Clone, Debug)]
pub struct PropEntry {
    pub start: Position<usize>,
    pub end: Position<usize>,
    /// Style to apply. For face-only entries, restyles the underlying chars;
    /// for display entries, styles the substituted content. Stored as a raw
    /// `Value` so the face name resolves through the theme at render time
    /// (a theme reload updates existing entries).
    pub face: Option<Rc<Value>>,
    /// When set, the renderer *replaces* the range with this content instead
    /// of restyling. Only honored for single-row ranges (`start.row ==
    /// end.row`); multi-row display would require hiding subsequent rows,
    /// which is out of scope for this pipeline.
    pub display: Option<Display>,
    /// Overlays only; ignored for text properties. Higher wins.
    pub priority: i64,
    /// When set, the highlight pads to the area's full width on each row it
    /// spans — same flag as `StyledRange::pad_to_width`.
    pub pad_to_width: bool,
}

/// Per-buffer property/overlay storage. Held inline on [`Buffer`]; cloned
/// when the buffer is cloned (cheap — entries are small).
#[derive(Clone, Debug, Default)]
pub struct PropStore {
    /// Anonymous range annotations. Order is insertion order; rendering
    /// applies them in this order so later entries layer over earlier ones.
    pub text_properties: Vec<PropEntry>,
    /// Named overlays keyed by `OverlayId`. The id space is per-buffer.
    pub overlays: Vec<(OverlayId, PropEntry)>,
    next_id: u64,
}

/// Opaque handle for an overlay, returned by `overlay-create`. Wraps a `u64`
/// that's stable for the lifetime of the buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OverlayId(pub u64);

impl PropStore {
    pub fn push_text_property(&mut self, e: PropEntry) {
        self.text_properties.push(e);
    }

    pub fn clear_text_properties(&mut self) {
        self.text_properties.clear();
    }

    pub fn create_overlay(&mut self, e: PropEntry) -> OverlayId {
        let id = OverlayId(self.next_id);
        self.next_id += 1;
        self.overlays.push((id, e));
        id
    }

    pub fn overlay_mut(&mut self, id: OverlayId) -> Option<&mut PropEntry> {
        self.overlays
            .iter_mut()
            .find_map(|(oid, e)| (*oid == id).then_some(e))
    }

    pub fn delete_overlay(&mut self, id: OverlayId) -> bool {
        let before = self.overlays.len();
        self.overlays.retain(|(oid, _)| *oid != id);
        self.overlays.len() < before
    }
}

// ---------------------------------------------------------------------------
// Range emission
// ---------------------------------------------------------------------------

/// Walk a buffer's text properties + overlays and produce render-ready
/// [`StyledRange`]s clipped to the visible viewport. Order in the returned
/// `DecoratorRanges` is `text-properties → overlays sorted by ascending
/// priority`, so higher-priority overlays naturally apply last (= on top).
pub fn build_prop_ranges(buf: &Buffer, theme: &Theme) -> DecoratorRanges {
    let mut ranges = Vec::new();
    let start_row = buf.file_pos().row;
    let visible_rows = buf.viewport.row as usize;
    if visible_rows == 0 {
        return DecoratorRanges { ranges };
    }
    let end_row_excl = start_row + visible_rows;

    let store = buf.props();

    for e in &store.text_properties {
        emit_clipped(e, buf, theme, start_row, end_row_excl, &mut ranges);
    }

    // Sort overlays by priority ascending so higher priority is appended last.
    let mut ov: Vec<&PropEntry> = store.overlays.iter().map(|(_, e)| e).collect();
    ov.sort_by_key(|e| e.priority);
    for e in ov {
        emit_clipped(e, buf, theme, start_row, end_row_excl, &mut ranges);
    }

    DecoratorRanges { ranges }
}

fn emit_clipped(
    e: &PropEntry,
    buf: &Buffer,
    theme: &Theme,
    viewport_start: usize,
    viewport_end_excl: usize,
    out: &mut Vec<StyledRange>,
) {
    // Resolve style once for the entry. If unset, nothing to draw — text
    // properties currently only carry `face`.
    let Some(face_val) = e.face.as_ref() else {
        return;
    };
    let Ok(style) = style_from_value(face_val, theme) else {
        return;
    };

    let lo_row = e.start.row.max(viewport_start);
    let hi_row = e.end.row.min(viewport_end_excl.saturating_sub(1));
    if lo_row > hi_row {
        return;
    }

    for row in lo_row..=hi_row {
        // Per-row column span.
        let line_len = line_char_count(buf, row);
        let col = if row == e.start.row { e.start.col } else { 0 };
        let end_col = if row == e.end.row {
            e.end.col
        } else {
            line_len
        };
        let len = end_col.saturating_sub(col);
        // Display substitution is single-row only; only attach it to the
        // entry's first row (which here is also its only row when the entry
        // is single-row, since `start.row == end.row` constrains the loop).
        let display = if row == e.start.row && e.start.row == e.end.row {
            e.display.clone()
        } else {
            None
        };
        if len == 0 && !e.pad_to_width && display.is_none() {
            continue;
        }
        out.push(StyledRange {
            row,
            col,
            len,
            style: style.clone(),
            pad_to_width: e.pad_to_width,
            display,
        });
    }
}

fn line_char_count(buf: &Buffer, row: usize) -> usize {
    if buf.len_lines() < row {
        return 0;
    }
    buf.lines_at(row)
        .next()
        .map(|line| {
            line.to_string()
                .trim_end_matches(['\n', '\r'])
                .chars()
                .count()
        })
        .unwrap_or(0)
}
