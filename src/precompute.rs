//! Build a [`RenderedFrame`] from the editor's current state. Runs every
//! registered region (top/bottom strips, status segments, gutters,
//! decorators) under an `EditorGuard` installed by the caller, then layers
//! text-properties / overlays and the soft-wrap layout on top.
//!
//! The companion [`State::precompute_frame`] owns the guard/lisp dance and
//! delegates the actual computation here so the bulk of frame assembly is a
//! single, testable function over borrowed state.

use rizz::Env;

use crate::buffer::{Buffer, BufferKind};
use crate::keymap::KeyEvent;
use crate::regions::{
    RegionAnchor, RegionRegistry, produce_decorator, produce_gutter, produce_status_span,
    produce_strip_rows,
};
use crate::render::{
    CursorStyle, RenderedBuffer, RenderedFrame, RenderedStrip, StateSnapshot,
};
use crate::styling::ThemeCell;
use crate::window::WindowTree;

/// Inputs the precompute pass reads from `State`. All references are
/// immutable; the only mutation it performs is on its own local builders.
pub struct PrecomputeInput<'a> {
    pub bufs: &'a [Buffer],
    pub windows: &'a WindowTree,
    pub regions: &'a RegionRegistry,
    pub theme: &'a ThemeCell,
    pub focus_minibuffer: bool,
    pub minibuffer: usize,
    pub last_key: Option<KeyEvent>,
    pub lisp_env: &'a Env,
}

pub fn compute(input: PrecomputeInput<'_>) -> (RenderedFrame, Option<String>) {
    let PrecomputeInput {
        bufs,
        windows,
        regions,
        theme,
        focus_minibuffer,
        minibuffer,
        last_key,
        lisp_env,
    } = input;

    // Snapshot the theme so callbacks that mutate it (`face-define`) only
    // affect the next frame.
    let theme_snap = theme.borrow().clone();
    let default_style = theme_snap.resolve("default").unwrap_or_default();

    let mut error_chunks: Vec<String> = Vec::new();
    let record = |chunks: &mut Vec<String>, region_name: &str, err: rizz::RizzError| {
        if chunks.len() < 3 {
            chunks.push(format!("[{region_name}] {err}"));
        }
    };

    // Read-only snapshot status/strip producers consume.
    let snap = StateSnapshot {
        bufs,
        windows,
        minibuffer: &bufs[minibuffer],
        focus_minibuffer,
        bufno: windows.focused_bufno(),
        keyevent: last_key,
        cursor_style: CursorStyle::Block,
        popups: &[],
    };

    // One pass over the registry, routing each region's output to the
    // matching `RenderedFrame` bucket.
    let mut top_extra: Vec<RenderedStrip> = Vec::new();
    let mut bottom_extra: Vec<RenderedStrip> = Vec::new();
    let mut status_left: Vec<ratatui::text::Span<'static>> = Vec::new();
    let mut status_right: Vec<ratatui::text::Span<'static>> = Vec::new();
    for region in regions.iter() {
        match region.anchor {
            RegionAnchor::Top => match produce_strip_rows(region, &theme_snap, lisp_env) {
                Ok(lines) => top_extra.push(RenderedStrip { lines }),
                Err(e) => record(&mut error_chunks, &region.name, e),
            },
            RegionAnchor::Bottom => match produce_strip_rows(region, &theme_snap, lisp_env) {
                Ok(lines) => bottom_extra.push(RenderedStrip { lines }),
                Err(e) => record(&mut error_chunks, &region.name, e),
            },
            RegionAnchor::StatusLeft => {
                match produce_status_span(region, &snap, &theme_snap, lisp_env) {
                    Ok(spans) => status_left.extend(spans),
                    Err(e) => record(&mut error_chunks, &region.name, e),
                }
            }
            RegionAnchor::StatusRight => {
                match produce_status_span(region, &snap, &theme_snap, lisp_env) {
                    Ok(spans) => status_right.extend(spans),
                    Err(e) => record(&mut error_chunks, &region.name, e),
                }
            }
            // Per-buffer anchors are handled in the loop below.
            RegionAnchor::Gutter { .. } | RegionAnchor::Decorator => {}
        }
    }

    // Per-buffer pass: gutters + decorators, in registration order
    // (which is the renderer's left-to-right / layer order). Popup and
    // minibuffer kinds skip the region phase — their visual shell is
    // owned by their chrome / the minibuffer line — but they still get
    // a `prop_ranges` pass so text-property / overlay APIs work on a
    // popup buffer's content the same way they do on a file buffer.
    let mut per_buf = Vec::with_capacity(bufs.len());
    for (i, buf) in bufs.iter().enumerate() {
        let mut rb = RenderedBuffer::default();
        let participates_in_regions = i != minibuffer && buf.kind() != BufferKind::Popup;
        if participates_in_regions {
            for region in regions.iter() {
                match region.anchor {
                    RegionAnchor::Gutter { .. } => {
                        match produce_gutter(region, buf, &theme_snap, lisp_env) {
                            Ok(g) => rb.gutters.push(g),
                            Err(e) => record(&mut error_chunks, &region.name, e),
                        }
                    }
                    RegionAnchor::Decorator => {
                        match produce_decorator(region, buf, &theme_snap, lisp_env) {
                            Ok(d) => rb.decorators.push(d),
                            Err(e) => record(&mut error_chunks, &region.name, e),
                        }
                    }
                    _ => {}
                }
            }
        }
        // Text properties + overlays applied after user decorators so
        // they layer on top — overlays themselves are priority-ordered
        // inside `build_prop_ranges`. Run for popups too.
        if i != minibuffer {
            let prop_ranges = crate::props::build_prop_ranges(buf, &theme_snap);
            if !prop_ranges.ranges.is_empty() {
                rb.decorators.push(prop_ranges);
            }
        }

        // Soft-wrap layout. Built after gutters so the wrap width is
        // the actual content area (viewport - gutters). Bail early when
        // wrap is off, the viewport hasn't been sized yet, or gutters
        // ate the whole row.
        if !matches!(buf.wrap_mode(), crate::wrap::WrapMode::None) && buf.viewport.row > 0 {
            let gutter_w: u16 = rb.gutters.iter().map(|g| g.width).sum();
            let content_w = buf
                .wrap_column()
                .unwrap_or_else(|| buf.viewport.col.saturating_sub(gutter_w));
            if content_w > 0 {
                let cfg = crate::wrap::WrapConfig {
                    mode: buf.wrap_mode(),
                    width: content_w,
                    breakindent: buf.breakindent(),
                };
                // Build a generous cache (several screenfuls) so single-step
                // movements like `j` near the viewport bottom can step
                // onto the next visual row without falling off the map.
                let budget = ((buf.viewport.row as usize) * 4).max(200);
                let map = crate::wrap::WrapMap::build(buf, buf.file_pos().row, budget, cfg);
                rb.wrap = Some(map);
            }
        }
        per_buf.push(rb);
    }

    let error_msg = if error_chunks.is_empty() {
        None
    } else {
        Some(error_chunks.join("; "))
    };

    (
        RenderedFrame {
            default_style,
            theme: theme_snap,
            top_extra,
            status_left,
            status_right,
            bottom_extra,
            per_buf,
        },
        error_msg,
    )
}
