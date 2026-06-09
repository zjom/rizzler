//! Rendering: theme, frame/gutter lisp callbacks, the per-tick render pass.
//!
//! `precompute_frame` walks every region under an `EditorGuard` and packs the
//! results into a `RenderedFrame` the renderer consumes without ever touching
//! lisp. `render` snapshots the editor state for the renderer plus surfaces
//! any callback errors via `notify_via_lisp`.

use std::io;
use std::rc::Rc;

use rizz::runtime::Value;
use rizz_core::EditingMode;
use rizz_text::BufferId;
use rizz_ui::{
    Renderer, StateSnapshot, ThemeCell, precompute,
    render::{CursorStyle, GutterWidth, RenderedFrame},
};
use tracing::{error, instrument, warn};

use crate::lisp::{EditorGuard, RenderPhaseGuard};

use super::State;

/// Rendering plumbing: the terminal renderer, the named-style theme, and the
/// lisp callbacks that build the per-tick frame and gutter. `precompute_frame`
/// hands the callbacks to the precompute pass; `render` feeds the resulting
/// frame to the renderer.
pub(super) struct Render {
    pub renderer: Box<dyn Renderer>,
    /// Named styles registered by lisp (`face-define`). `RefCell` so render
    /// callbacks can introspect without holding `&mut State`.
    pub theme: ThemeCell,
    /// Lisp callable that builds the frame's widget tree each render.
    pub frame_fn: Option<Rc<Value>>,
    /// Lisp callable + width policy that renders the per-row gutter for every
    /// file buffer. `gutter_fn = None` = no gutter. Set by the lisp
    /// `set-gutter` builtin and consumed by the precompute pass. Width
    /// defaults to [`GutterWidth::Fit`].
    pub gutter_fn: Option<Rc<Value>>,
    pub gutter_width: GutterWidth,
}

impl Render {
    pub(super) fn new(renderer: Box<dyn Renderer>) -> Self {
        Self {
            renderer,
            theme: ThemeCell::default(),
            frame_fn: None,
            gutter_fn: None,
            gutter_width: GutterWidth::Fit,
        }
    }
}

impl State {
    pub fn theme(&self) -> &ThemeCell {
        &self.render.theme
    }

    pub fn set_frame_fn(&mut self, f: Option<Rc<Value>>) {
        self.render.frame_fn = f;
    }

    pub fn get_frame_fn(&self) -> Option<&Rc<Value>> {
        self.render.frame_fn.as_ref()
    }

    /// Install the per-frame gutter callback. `width` is the column policy
    /// reserved on the left of every file buffer ([`GutterWidth::Fit`] sizes
    /// to the widest row, [`GutterWidth::Fixed`] reserves a constant count);
    /// pass `None` for `f` to disable the gutter entirely.
    pub fn set_gutter(&mut self, f: Option<Rc<Value>>, width: GutterWidth) {
        self.render.gutter_fn = f;
        self.render.gutter_width = width;
    }

    pub fn gutter_fn(&self) -> Option<&Rc<Value>> {
        self.render.gutter_fn.as_ref()
    }

    pub fn gutter_width(&self) -> GutterWidth {
        self.render.gutter_width
    }

    #[instrument(skip(self))]
    pub fn render(&mut self) -> io::Result<()> {
        let focused = self.focused_buf_id();
        let (frame, error_msg) = self.precompute_frame();
        let writebacks: Vec<(BufferId, Option<rizz_text::WrapMap>)> = frame
            .per_buf
            .iter()
            .map(|(id, rb)| (id, rb.wrap.clone()))
            .collect();
        for (id, wrap) in writebacks {
            if let Some(buf) = self.bufs.get_mut(id) {
                buf.set_wrap_cache(wrap);
            }
        }
        let snap = StateSnapshot {
            bufs: self.bufs.raw(),
            windows: &self.surface.windows,
            minibuffer: self.bufs.minibuffer(),
            buf: self.surface.windows.focused_buf(),
            keyevent: self.input.keyevents.peek_back().map(|(e, _)| e.to_owned()),
            cursor_style: match self.bufs[focused].mode() {
                EditingMode::Insert | EditingMode::Command => CursorStyle::Bar,
                _ => CursorStyle::Block,
            },
            panels: &self.surface.panels,
        };
        let result = self.render.renderer.render(snap, &frame);
        if let Err(e) = &result {
            error!(error = %e, "renderer.render failed");
        }
        if let Some(msg) = error_msg {
            warn!(msg = %msg, "precompute reported an error -> notifying via lisp");
            self.notify_via_lisp(&msg);
        }
        result
    }

    /// Run every region under an `EditorGuard`, packing the results into a
    /// `RenderedFrame` the renderer can consume without ever touching lisp.
    #[instrument(skip(self))]
    pub fn precompute_frame(&mut self) -> (RenderedFrame, Option<String>) {
        // Sync syntax trees before precompute walks buffers immutably.
        // `refresh_highlight` short-circuits when the tree is already clean.
        for (_, b) in self.bufs.iter_mut() {
            b.refresh_highlight();
        }

        let lisp = self
            .scripting
            .lisp
            .take()
            .expect("recursive render is not supported");
        let _editor_guard = EditorGuard::new(self);
        let _phase_guard = RenderPhaseGuard::enter();

        let result = precompute::compute(precompute::PrecomputeInput {
            bufs: self.bufs.raw(),
            windows: &self.surface.windows,
            frame_fn: self.render.frame_fn.as_ref(),
            theme: &self.render.theme,
            minibuffer: self.bufs.minibuffer_id(),
            file_bufs: self.bufs.file_ids(),
            gutter: self.render.gutter_fn.as_ref(),
            gutter_width: self.render.gutter_width,
            lisp_env: lisp.env(),
        });

        drop(_phase_guard);
        drop(_editor_guard);
        self.scripting.lisp = Some(lisp);

        result
    }
}
