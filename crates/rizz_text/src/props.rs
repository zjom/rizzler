//! Text properties and overlays.
//!
//! A **text property** is a buffer-resident annotation: it covers an absolute
//! range `[start, end)` (row+col positions) and carries a property bag
//! (currently `face` + `display`, but the shape is extensible). Properties
//! don't auto-shift through edits in this version — callers are expected to
//! clear and re-apply when the buffer changes.
//!
//! An **overlay** is the same shape but addressed by a stable handle so lisp
//! can mutate or remove individual entries without re-emitting the whole set.
//! Overlays have a `priority`; among overlapping overlays, higher priority
//! wins.
//!
//! Range emission (walking these into renderer-shaped `StyledRange`s) lives
//! in `rizz_ui::precompute` so this crate doesn't take a UI dependency.

use std::rc::Rc;

use rizz::runtime::Value;
use rizz_core::{Display, Position};

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
    /// spans.
    pub pad_to_width: bool,
}

/// Per-buffer property/overlay storage. Held inline on [`crate::Buffer`];
/// cloned when the buffer is cloned (cheap — entries are small).
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
