//! Surface management: popups (overlay panels), the minibuffer panel,
//! viewport sizing, and mode switching that involves the panel stack.
//!
//! Editor windows are not panels; when the panel stack is empty, focus falls
//! through to the focused window leaf.

use std::rc::Rc;

use rizz_core::{EditingMode, Position};
use rizz_search::SearchOrigin;
use rizz_text::BufferId;
use rizz_ui::{
    WindowTree,
    panel::{Panel, PanelKind, PanelStack},
};
use tracing::{debug, info, instrument, trace};

use super::{PopupSpec, State};

/// The arrangement of editor windows + the panel stack stacked above them.
/// Windows form a layout tree of buffer leaves; panels (minibuffer when a
/// `Command` / `Search` mode is active, popups, …) steal focus from the
/// underlying window when present.
pub(super) struct Surface {
    pub windows: WindowTree,
    pub panels: PanelStack,
}

impl Surface {
    pub(super) fn new(focused_buf: BufferId) -> Self {
        Self {
            windows: WindowTree::new(focused_buf),
            panels: PanelStack::new(),
        }
    }
}

/// Bottom-of-screen reservation: one row for the status line, one for the
/// minibuffer. Subtracted from the terminal height when sizing the editor
/// area for the window tree.
pub(super) const STATUS_LINE_ROWS: u16 = 1;
pub(super) const MINIBUFFER_ROWS: u16 = 1;

impl State {
    /// Topmost keymap layer of the topmost *overlay* panel, if any. Used by
    /// the `popup-mode` lisp builtin to detect "am I inside a popup of kind X?"
    pub fn top_popup_mode(&self) -> Option<Rc<str>> {
        self.surface.panels
            .top_overlay()
            .and_then(|p| p.keymap_layers.last().cloned())
    }

    /// Show the overlay panel named `name`. If a panel with this name is
    /// already on the stack, update its widget / placement / options in
    /// place (preserving its backing buffer's text unless `spec.initial_text`
    /// is set) and re-raise it to the top. Otherwise create a fresh
    /// panel-backing buffer, append it to `self.bufs`, and push a new
    /// overlay panel. Returns the backing buffer's `BufferId` in both cases.
    #[instrument(skip(self, spec), fields(
        modes = ?spec.mode_layers,
        buffer_mode = ?spec.buffer_mode,
        wrap_mode = ?spec.wrap_mode,
    ))]
    pub fn show_popup(&mut self, name: Rc<str>, spec: PopupSpec) -> BufferId {
        if let Some(mut existing) = self.surface.panels.remove_overlay_by_name(&name) {
            let id = existing.buf;
            let buf = &mut self.bufs[id];
            if let Some(text) = spec.initial_text {
                buf.clear_with(&text);
            }
            buf.set_mode(spec.buffer_mode);
            buf.set_wrap_mode(spec.wrap_mode);
            buf.set_wrap_column(spec.wrap_column);
            buf.set_breakindent(spec.breakindent);
            existing.keymap_layers = spec.mode_layers;
            existing.widget = Some(spec.widget);
            existing.kind = PanelKind::Overlay {
                placement: spec.placement,
                show_cursor: spec.show_cursor,
                name,
            };
            self.surface.panels.push(existing);
            self.refresh_viewport();
            info!(?id, "overlay panel updated");
            return id;
        }
        let mut buf = rizz_text::Buffer::new();
        if let Some(text) = spec.initial_text {
            buf.clear_with(&text);
        }
        buf.set_mode(spec.buffer_mode);
        buf.set_wrap_mode(spec.wrap_mode);
        buf.set_wrap_column(spec.wrap_column);
        buf.set_breakindent(spec.breakindent);
        let id = self.bufs.push_panel(buf);
        self.surface.panels.push(Panel {
            buf: id,
            keymap_layers: spec.mode_layers,
            widget: Some(spec.widget),
            kind: PanelKind::Overlay {
                placement: spec.placement,
                show_cursor: spec.show_cursor,
                name,
            },
        });
        self.refresh_viewport();
        info!(?id, "overlay panel opened");
        id
    }

    /// Close the named overlay panel. Returns true if such a panel was
    /// open. Frees its backing buffer (unless that buffer is a file or the
    /// minibuffer).
    #[instrument(skip(self))]
    pub fn hide_popup(&mut self, name: &str) -> bool {
        let Some(panel) = self.surface.panels.remove_overlay_by_name(name) else {
            trace!(name, "no overlay with that name");
            return false;
        };
        self.dispose_overlay_buf(panel.buf);
        self.refresh_viewport();
        true
    }

    /// Close the topmost overlay panel (skipping a minibuffer panel if it
    /// sits on top). Keyed off "topmost" rather than name, so generic
    /// dismiss bindings (`q` / `<esc>` in the `popup` keymap layer) don't
    /// need to know which popup they're closing.
    #[instrument(skip(self))]
    pub fn close_popup(&mut self) -> bool {
        let Some(panel) = self.surface.panels.pop_top_overlay() else {
            trace!("no overlay to close");
            return false;
        };
        self.dispose_overlay_buf(panel.buf);
        self.refresh_viewport();
        true
    }

    fn dispose_overlay_buf(&mut self, removed: BufferId) {
        info!(?removed, "closing overlay");
        // File buffers outlive their panel — closing a popup viewing file
        // buf 2 must not delete file buf 2.
        if !self.bufs.is_file_buf(removed) && removed != self.bufs.minibuffer_id() {
            self.bufs.remove(removed);
            let first = self.bufs.first_file_buf();
            self.surface.windows.for_each_leaf_mut(|b| {
                if *b == removed {
                    *b = first;
                }
            });
        }
    }

    pub fn top_popup_buf(&self) -> Option<BufferId> {
        self.surface.panels.top_overlay().map(|p| p.buf)
    }

    /// Look up a named popup's backing buffer. Returns `None` if no popup
    /// with that name is currently on the stack.
    pub fn popup_buf_by_name(&self, name: &str) -> Option<BufferId> {
        self.surface.panels.iter().find_map(|p| match &p.kind {
            PanelKind::Overlay { name: n, .. } if n.as_ref() == name => Some(p.buf),
            _ => None,
        })
    }

    pub fn has_popup(&self) -> bool {
        self.surface.panels.any_overlay()
    }

    /// Update viewports of all buffers currently displayed in a window,
    /// the minibuffer, and every overlay panel.
    pub(super) fn refresh_viewport(&mut self) {
        let Ok((cols, rows)) = crossterm::terminal::size() else {
            return;
        };
        let editor_h = rows.saturating_sub(STATUS_LINE_ROWS + MINIBUFFER_ROWS);
        let editor_area = ratatui::layout::Rect::new(0, 0, cols, editor_h);
        let focused_path = self.surface.windows.focused_path().clone();
        let mut cursor_anchor: Option<rizz_ui::panel::CursorAnchor> = None;
        for leaf in self.surface.windows.layout(editor_area) {
            if let Some(buf) = self.bufs.get_mut(leaf.buf) {
                buf.viewport = Position::new(leaf.area.width, leaf.area.height);
            }
            if leaf.path == focused_path {
                // Approximate cursor frame coords — the rendering path adds
                // the gutter offset, but a small column offset doesn't matter
                // for placement-size selection.
                if let Some(buf) = self.bufs.get(leaf.buf) {
                    let c = buf.cursor_pos();
                    cursor_anchor = Some(rizz_ui::panel::CursorAnchor {
                        leaf: leaf.area,
                        cursor_x: leaf.area.x.saturating_add(c.col),
                        cursor_y: leaf.area.y.saturating_add(c.row),
                    });
                }
            }
        }
        self.bufs.minibuffer_mut().viewport = Position::new(cols, MINIBUFFER_ROWS);
        let overlay_viewports: Vec<(BufferId, Position<u16>)> = self
            .surface
            .panels
            .overlays()
            .filter_map(|p| {
                let (_, widget, _) = p.as_overlay()?;
                let buf = &self.bufs[p.buf];
                let outer =
                    rizz_ui::panel::resolve_overlay_rect(p, editor_area, buf, cursor_anchor);
                let inner = rizz_ui::panel::buffer_view_rect(widget, outer, p.buf);
                Some((p.buf, Position::new(inner.width, inner.height)))
            })
            .collect();
        for (id, viewport) in overlay_viewports {
            if let Some(buf) = self.bufs.get_mut(id) {
                buf.viewport = viewport;
            }
        }
    }

    pub(super) fn set_mode(&mut self, mode: EditingMode) {
        if matches!(mode, EditingMode::Command | EditingMode::Search) {
            debug!(?mode, "entering minibuffer-backed mode");
            // Stash cursor + scroll for cancel-restore and live-search origin.
            if mode == EditingMode::Search {
                let buf_id = self.focused_buf_id();
                let buf = &self.bufs[buf_id];
                self.search.set_origin(SearchOrigin {
                    buf: buf_id,
                    cursor: buf.abs_pos(),
                });
            }
            self.bufs.minibuffer_mut().clear();
            self.bufs.minibuffer_mut().set_mode(mode);
            // Re-entering while already in a minibuffer mode is a no-op.
            if !self.surface.panels.iter().any(|p| p.is_minibuffer()) {
                let mb = self.bufs.minibuffer_id();
                self.surface.panels.push(Panel::minibuffer(mb));
            }
        } else {
            let f = self.focused_buf_id();
            debug!(buf = ?f, ?mode, "setting buffer mode");
            self.bufs[f].set_mode(mode);
        }
    }

    pub(super) fn exit_minibuffer(&mut self) {
        debug!("exiting minibuffer");
        self.bufs.minibuffer_mut().clear();
        self.bufs.minibuffer_mut().set_mode(EditingMode::Command);
        self.surface.panels.pop_minibuffer();
        let editor = self.surface.windows.focused_buf();
        self.bufs[editor].set_mode(EditingMode::Normal);
    }
}
