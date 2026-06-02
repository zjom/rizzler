use crate::keymap::KeyEvent;
use std::path::Path;
use std::rc::Rc;
use std::time::Instant;
use std::{io, time::Duration};

use crossterm::event::KeyEvent as CTKeyEvent;
use ringbuffer::RingBuffer;
use rizz::RizzError;
use rizz::runtime::Value;

use crate::{
    action::Action,
    buffer::{Buffer, BufferKind},
    keymap::KeymapRegistry,
    lisp::{EditorGuard, LispRuntime, init_script_path},
    mode::EditingMode,
    position::Position,
    render::{CursorStyle, Renderer, StateSnapshot},
    render_ratatui::RatatuiRenderer,
    slots::SlotRegistry,
    styling::ThemeCell,
    window::{SplitDir, WindowTree},
};

/// Bottom-of-screen reservation: one row for the status line, one for the
/// minibuffer. Subtracted from the terminal height when sizing the editor
/// area for the window tree.
const STATUS_LINE_ROWS: u16 = 1;
const MINIBUFFER_ROWS: u16 = 1;
const KEYCOMBO_TIMEOUT: Duration = Duration::from_millis(100);

/// Bundle of plugin points injected into [`State`]. Swap any field to
/// customise the editor without touching `State`'s internals.
pub struct Config {
    pub renderer: Box<dyn Renderer>,
    pub keycombo_timeout: Duration,
}

impl Config {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            renderer: Box::new(RatatuiRenderer::new()?),
            keycombo_timeout: KEYCOMBO_TIMEOUT,
        })
    }
}

pub struct State {
    /// All live buffers. Index 0 is the minibuffer by construction; file
    /// buffers occupy index 1..
    bufs: Vec<Buffer>,
    /// Tree of editor windows. Leaves point at indices into `bufs`. The
    /// minibuffer is not part of this tree.
    windows: WindowTree,
    /// When true, key events route to the minibuffer instead of the focused
    /// editor window.
    focus_minibuffer: bool,
    /// Index of the minibuffer (always kind = Minibuffer).
    minibuffer: usize,
    quit: bool,
    keymap: KeymapRegistry,
    keyevents: RingBuffer<(KeyEvent, Instant), 100>,
    keycombo_timeout: Duration,
    renderer: Box<dyn Renderer>,
    /// Embedded lisp runtime. Held as `Option` so `eval_lisp*` can `take` it
    /// for the duration of an eval — this also blocks re-entrant evaluation.
    lisp: Option<LispRuntime>,
    /// Named styles registered by lisp (`face-define`). `RefCell` so render
    /// callbacks can introspect without holding `&mut State`.
    theme: ThemeCell,
    /// Ordered registry of customization slots (status segments, gutters,
    /// line decorators, bottom-strip components). Owned, not RefCell — only
    /// the lisp builtins that hold `&mut State` mutate it.
    slots: SlotRegistry,
    workdir: Rc<Path>,
}

impl State {
    pub fn new() -> io::Result<Self> {
        Self::with_config(Config::new()?)
    }

    pub fn with_config(config: Config) -> io::Result<Self> {
        let workdir = std::env::current_dir()?;
        // Layout: [minibuffer, first file buffer]. The window tree starts as
        // a single leaf pointing at the file buffer.
        let mut state = Self {
            bufs: vec![Buffer::minibuffer(), Buffer::new()],
            windows: WindowTree::new(1),
            focus_minibuffer: false,
            minibuffer: 0,
            quit: false,
            keymap: KeymapRegistry::new(),
            keyevents: RingBuffer::new(),
            keycombo_timeout: config.keycombo_timeout,
            renderer: config.renderer,
            lisp: Some(LispRuntime::new()),
            theme: ThemeCell::default(),
            slots: SlotRegistry::new(),
            workdir: workdir.into(),
        };
        state.refresh_viewport();

        // Bundled defaults: keybindings, then visual configuration, then
        // (optional) user `init.lisp`. Each layer can override the previous.
        if let Err(e) = state.eval_lisp_script(include_str!("../default.lisp")) {
            eprintln!("default.lisp eval failed: {e}");
        }
        if let Err(e) = state.eval_lisp_script(include_str!("../default-style.lisp")) {
            eprintln!("default-style.lisp eval failed: {e}");
        }
        if let Some(path) = init_script_path()
            && let Ok(src) = std::fs::read_to_string(&path)
            && let Err(e) = state.eval_lisp_script(&src)
        {
            eprintln!("init.lisp ({}) eval failed: {e}", path.display());
        }

        Ok(state)
    }

    /// Parse `src` as one lisp form and evaluate it in the embedded runtime.
    /// Re-entrant calls panic — see [`crate::lisp::EditorGuard`].
    pub fn eval_lisp(&mut self, src: &str) -> Result<Rc<Value>, RizzError> {
        let mut lisp = self
            .lisp
            .take()
            .expect("recursive eval_lisp is not supported");
        let result = {
            let _guard = EditorGuard::new(self);
            lisp.eval_str(src)
        };
        self.lisp = Some(lisp);
        result
    }

    /// Evaluate an already-parsed form. Used to dispatch keymap-bound lisp.
    pub fn eval_lisp_value(&mut self, form: Rc<Value>) -> Result<Rc<Value>, RizzError> {
        let mut lisp = self
            .lisp
            .take()
            .expect("recursive eval_lisp is not supported");
        let result = {
            let _guard = EditorGuard::new(self);
            lisp.eval_value(form)
        };
        self.lisp = Some(lisp);
        result
    }

    /// Evaluate a multi-form script (e.g. `default.lisp`, `init.lisp`).
    pub fn eval_lisp_script(&mut self, src: &str) -> Result<(), RizzError> {
        let mut lisp = self
            .lisp
            .take()
            .expect("recursive eval_lisp is not supported");
        let result = {
            let _guard = EditorGuard::new(self);
            lisp.eval_script(src)
        };
        self.lisp = Some(lisp);
        result
    }

    /// Read-only accessor for the focused buffer. Exposed for lisp builtins
    /// that need to query buffer state without going through `Action`.
    pub(crate) fn focused_buf(&self) -> &Buffer {
        let i = self.focused_bufno();
        &self.bufs[i]
    }

    /// Accessor for the [`crate::styling::Theme`] cell. Used by `face-define`
    /// / `face-of` builtins.
    pub(crate) fn theme(&self) -> &ThemeCell {
        &self.theme
    }

    /// Mutable accessor for the slot registry. Used by the `*-add`/`*-remove`
    /// builtins.
    pub(crate) fn slots_mut(&mut self) -> &mut SlotRegistry {
        &mut self.slots
    }

    /// Write `msg` into the minibuffer as a status line. Used by lisp's
    /// `(message ...)` and as the sink for eval errors.
    pub(crate) fn set_minibuffer_message(&mut self, msg: &str) {
        let b = &mut self.bufs[self.minibuffer];
        b.clear();
        for c in msg.chars() {
            b.insert_char(c);
        }
    }

    /// Read the current minibuffer text and leave the minibuffer. Used by the
    /// `command-submit` lisp builtin, which then evaluates the text inline
    /// using the env already in scope (calling back into `eval_lisp` from here
    /// would re-take `self.lisp` and panic).
    pub(crate) fn take_minibuffer_command(&mut self) -> String {
        let cmd = self.bufs[self.minibuffer].text();
        self.exit_minibuffer();
        cmd
    }

    pub(crate) fn workdir(&self) -> Rc<Path> {
        self.workdir.clone()
    }

    /// The buffer currently receiving key events.
    fn focused_bufno(&self) -> usize {
        if self.focus_minibuffer {
            self.minibuffer
        } else {
            self.windows.focused_bufno()
        }
    }

    /// Update viewports of all buffers currently displayed in a window plus
    /// the minibuffer. Per-leaf rect comes from the window tree layout.
    /// Silently ignores terminal::size errors so tests without a real TTY
    /// still work.
    fn refresh_viewport(&mut self) {
        let Ok((cols, rows)) = crossterm::terminal::size() else {
            return;
        };
        let editor_h = rows.saturating_sub(STATUS_LINE_ROWS + MINIBUFFER_ROWS);
        let editor_area = ratatui::layout::Rect::new(0, 0, cols, editor_h);
        for leaf in self.windows.layout(editor_area) {
            if let Some(buf) = self.bufs.get_mut(leaf.bufno) {
                buf.viewport = Position::new(leaf.area.width, leaf.area.height);
            }
        }
        self.bufs[self.minibuffer].viewport = Position::new(cols, MINIBUFFER_ROWS);
    }

    pub fn quit_requested(&self) -> bool {
        self.quit
    }

    pub fn handle_key_event(&mut self, event: CTKeyEvent) -> io::Result<()> {
        let now = Instant::now();
        let timedout = self
            .keyevents
            .peek_back()
            .is_some_and(|(_, earlier)| now.duration_since(*earlier) > self.keycombo_timeout);
        self.keyevents.push_back((event.into(), now));

        let mode = self.bufs[self.focused_bufno()].mode();
        if let Some(action) = self
            .keymap
            .resolve(mode.to_str().into(), event.into(), timedout)
        {
            self.apply(&action)?;
        }
        // Refresh after apply: window splits/closes and buffer switches may
        // have changed which buffer occupies which viewport.
        self.refresh_viewport();
        let focused = self.focused_bufno();
        self.bufs[focused].clamp_cursor();
        self.render()
    }

    pub fn apply(&mut self, actions: &[Rc<Action>]) -> io::Result<()> {
        for action in actions {
            match action.as_ref() {
                Action::Noop => {}
                Action::Quit => self.quit = true,
                Action::SetMode(m) => self.set_mode(*m),
                Action::InsertChar(c) => {
                    let f = self.focused_bufno();
                    self.bufs[f].insert_char(*c);
                }
                Action::InsertNewline => {
                    let f = self.focused_bufno();
                    self.bufs[f].insert_char('\n');
                }
                Action::DeleteChar => {
                    let f = self.focused_bufno();
                    self.bufs[f].delete_char();
                }
                Action::MoveCursor(m) => {
                    let f = self.focused_bufno();
                    self.bufs[f].move_cursor(*m);
                }
                Action::CommandCancel => self.exit_minibuffer(),
                Action::BufCreate { path, set_active } => {
                    self.create_buf(*set_active, path.clone())?;
                }
                Action::BufDelete => {
                    let editor = self.windows.focused_bufno();
                    self.delete_buf(editor);
                }
                Action::BufNext => self.next_buffer(),
                Action::BufPrev => self.previous_buffer(),
                Action::BufEdit(path) => {
                    self.edit_buf(path.clone())?;
                }
                Action::BufWrite(path) => self.write_buf(path.clone())?,
                Action::WindowSplit(dir) => self.window_split(*dir),
                Action::WindowClose => self.window_close(),
                Action::WindowFocusNext => self.windows.focus_next(),
                Action::WindowFocus(d) => self.windows.focus_dir(*d),
                Action::KeymapSet { mode, lhs, rhs } => {
                    self.keymap.set(mode.to_str().into(), lhs, rhs.clone());
                }
                Action::KeymapRemove { mode, lhs } => {
                    self.keymap.remove(mode.to_str().into(), lhs);
                }
                Action::EvalLisp(form) => {
                    if let Err(e) = self.eval_lisp_value(form.clone()) {
                        self.set_minibuffer_message(&e.to_string());
                    }
                }
            }
        }
        Ok(())
    }

    /// Apply a SetMode action. Command is special: it moves focus to the
    /// minibuffer instead of changing an editor buffer's mode.
    fn set_mode(&mut self, mode: EditingMode) {
        if mode == EditingMode::Command {
            // Wipe any leftover status text from a previous eval.
            self.bufs[self.minibuffer].clear();
            self.bufs[self.minibuffer].set_mode(EditingMode::Command);
            self.focus_minibuffer = true;
        } else {
            let f = self.focused_bufno();
            self.bufs[f].set_mode(mode);
        }
    }

    /// Clear the minibuffer, drop focus from it, and reset the focused
    /// editor buffer to Normal mode.
    fn exit_minibuffer(&mut self) {
        self.bufs[self.minibuffer].clear();
        self.bufs[self.minibuffer].set_mode(EditingMode::Command);
        self.focus_minibuffer = false;
        let editor = self.windows.focused_bufno();
        self.bufs[editor].set_mode(EditingMode::Normal);
    }

    fn create_buf(&mut self, set_active: bool, path: Option<Rc<Path>>) -> io::Result<usize> {
        let buf = match path {
            Some(ref p) => self
                .bufs
                .iter()
                .find(|b| b.fs_path == path)
                .cloned()
                .unwrap_or_else(|| Buffer::with_path(p.clone())),
            None => Buffer::new(),
        };

        self.bufs.push(buf);
        let bufno = self.bufs.len() - 1;
        if set_active {
            self.windows.set_focused_bufno(bufno);
        }
        Ok(bufno)
    }

    fn edit_buf(&mut self, path: Rc<Path>) -> io::Result<usize> {
        let idx = self
            .bufs
            .iter()
            .position(|b| b.fs_path.as_ref() == Some(&path));
        match idx {
            Some(idx) => {
                self.windows.set_focused_bufno(idx);
                Ok(idx)
            }
            None => {
                self.bufs.push(Buffer::with_path(path));
                let idx = self.bufs.len() - 1;
                self.windows.set_focused_bufno(idx);
                Ok(idx)
            }
        }
    }

    fn write_buf(&mut self, path: Option<Rc<Path>>) -> io::Result<()> {
        let editor = self.windows.focused_bufno();
        self.bufs[editor].write(path)
    }

    /// Refuses to delete the minibuffer; keeps at least one file buffer alive.
    fn delete_buf(&mut self, bufno: usize) {
        if bufno >= self.bufs.len() || self.bufs[bufno].kind() == BufferKind::Minibuffer {
            return;
        }

        if self.file_buf_count() == 1 {
            // Last file buffer: reset it in place instead of removing.
            self.bufs[bufno] = Buffer::new();
            self.windows.for_each_leaf_mut(|b| *b = bufno);
            return;
        }

        self.bufs.remove(bufno);
        if self.minibuffer > bufno {
            self.minibuffer -= 1;
        }
        // Reindex every leaf that pointed past the removed buffer; any leaf
        // that pointed AT the removed buffer falls back to the first file buf.
        let first = self.first_file_buf();
        self.windows.for_each_leaf_mut(|b| {
            if *b == bufno {
                *b = first;
            } else if *b > bufno {
                *b -= 1;
            }
        });
    }

    fn window_split(&mut self, dir: SplitDir) {
        // New pane gets a fresh scratch buffer.
        self.bufs.push(Buffer::new());
        let new_bufno = self.bufs.len() - 1;
        self.windows.split(dir, new_bufno);
    }

    fn window_close(&mut self) {
        self.windows.close_focused();
    }

    fn file_buf_count(&self) -> usize {
        self.bufs
            .iter()
            .filter(|b| b.kind() != BufferKind::Minibuffer)
            .count()
    }

    fn first_file_buf(&self) -> usize {
        self.bufs
            .iter()
            .position(|b| b.kind() != BufferKind::Minibuffer)
            .expect("at least one file buffer always exists")
    }

    /// Cycle the focused window to the previous file buffer in `bufs`,
    /// skipping the minibuffer.
    fn previous_buffer(&mut self) {
        let n = self.bufs.len();
        let mut i = self.windows.focused_bufno();
        for _ in 0..n {
            i = if i == 0 { n - 1 } else { i - 1 };
            if self.bufs[i].kind() != BufferKind::Minibuffer {
                self.windows.set_focused_bufno(i);
                return;
            }
        }
    }

    fn next_buffer(&mut self) {
        let n = self.bufs.len();
        let mut i = self.windows.focused_bufno();
        for _ in 0..n {
            i = if i + 1 >= n { 0 } else { i + 1 };
            if self.bufs[i].kind() != BufferKind::Minibuffer {
                self.windows.set_focused_bufno(i);
                return;
            }
        }
    }

    pub fn render(&mut self) -> io::Result<()> {
        let focused = self.focused_bufno();
        // Build the precomputed frame first. This installs an `EditorGuard`
        // so lisp render callbacks can reach back into `State` for queries,
        // and runs each slot to a plain value the renderer can consume.
        let (frame, error_msg) = self.precompute_frame();
        let snap = StateSnapshot {
            bufs: &self.bufs,
            windows: &self.windows,
            minibuffer: &self.bufs[self.minibuffer],
            focus_minibuffer: self.focus_minibuffer,
            bufno: self.windows.focused_bufno(),
            keyevent: self.keyevents.peek_back().map(|(e, _)| e.to_owned()),
            cursor_style: match self.bufs[focused].mode() {
                EditingMode::Insert | EditingMode::Command => CursorStyle::Bar,
                _ => CursorStyle::Block,
            },
        };
        let result = self.renderer.render(snap, &frame);
        // Surface any render-callback error to the minibuffer *after* the
        // frame draws so the message itself isn't part of the failing pass.
        if let Some(msg) = error_msg {
            self.set_minibuffer_message(&msg);
        }
        result
    }

    /// Run every slot under an `EditorGuard`, packing the results into a
    /// `RenderedFrame` the renderer can consume without ever touching lisp.
    /// Returns the frame plus an optional error message (concatenated from
    /// the first few slot failures) to surface to the minibuffer.
    pub(crate) fn precompute_frame(&mut self) -> (crate::render::RenderedFrame, Option<String>) {
        use crate::render::{RenderedBottom, RenderedBuffer, RenderedFrame};
        use crate::slots::{
            SegmentSide, produce_bottom, produce_decorator, produce_gutter, produce_status_segment,
        };

        let lisp = self.lisp.take().expect("recursive render is not supported");
        let _editor_guard = crate::lisp::EditorGuard::new(self);
        let _phase_guard = crate::lisp::RenderPhaseGuard::enter();

        // Snapshot the theme so callbacks that mutate it (`face-define`) only
        // affect the next frame.
        let theme = self.theme.borrow().clone();
        let env = lisp.env().clone();

        let mut error_chunks: Vec<String> = Vec::new();
        let record = |chunks: &mut Vec<String>, slot_name: &str, err: rizz::RizzError| {
            if chunks.len() < 3 {
                chunks.push(format!("[{slot_name}] {err}"));
            }
        };

        // Build a synthetic snapshot for status/bottom slots. They take a
        // `&StateSnapshot`, which is just a read-only window onto fields
        // we already own.
        let snap = StateSnapshot {
            bufs: &self.bufs,
            windows: &self.windows,
            minibuffer: &self.bufs[self.minibuffer],
            focus_minibuffer: self.focus_minibuffer,
            bufno: self.windows.focused_bufno(),
            keyevent: self.keyevents.peek_back().map(|(ke, _)| ke.to_owned()),
            cursor_style: CursorStyle::Block, // placeholder; segments don't use it
        };

        // Status segments
        let mut status_left = Vec::new();
        for s in self.slots.status_segments(SegmentSide::Left) {
            match produce_status_segment(s, &snap, &theme, &env) {
                Ok(spans) => status_left.extend(spans),
                Err(e) => record(&mut error_chunks, &s.name, e),
            }
        }
        let mut status_right = Vec::new();
        for s in self.slots.status_segments(SegmentSide::Right) {
            match produce_status_segment(s, &snap, &theme, &env) {
                Ok(spans) => status_right.extend(spans),
                Err(e) => record(&mut error_chunks, &s.name, e),
            }
        }

        // Bottom rows (user-added only — status line and minibuffer are
        // hardcoded into the renderer's layout).
        let mut bottom_extra = Vec::new();
        for s in self.slots.bottom() {
            match produce_bottom(s, &snap, &theme, &env) {
                Ok(lines) => bottom_extra.push(RenderedBottom { lines }),
                Err(e) => record(&mut error_chunks, &s.name, e),
            }
        }

        // Per-buffer gutters and decorators.
        let mut per_buf = Vec::with_capacity(self.bufs.len());
        for (i, buf) in self.bufs.iter().enumerate() {
            // Skip the minibuffer — it has its own component.
            if i == self.minibuffer {
                per_buf.push(RenderedBuffer::default());
                continue;
            }
            let mut rb = RenderedBuffer::default();
            for s in self.slots.gutters() {
                match produce_gutter(s, buf, &theme, &env) {
                    Ok(g) => rb.gutters.push(g),
                    Err(e) => record(&mut error_chunks, &s.name, e),
                }
            }
            for s in self.slots.decorators() {
                match produce_decorator(s, buf, &theme, &env) {
                    Ok(d) => rb.decorators.push(d),
                    Err(e) => record(&mut error_chunks, &s.name, e),
                }
            }
            per_buf.push(rb);
        }

        // `runtime::apply` discards a callee's local bindings, so the env
        // hasn't moved. Drop both guards before restoring lisp.
        drop(_phase_guard);
        drop(_editor_guard);
        self.lisp = Some(lisp);

        let error_msg = if error_chunks.is_empty() {
            None
        } else {
            Some(error_chunks.join("; "))
        };

        (
            RenderedFrame {
                status_left,
                status_right,
                bottom_extra,
                per_buf,
            },
            error_msg,
        )
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// A renderer that does nothing — used in tests that don't want to touch a terminal.
    pub struct NullRenderer;
    impl Renderer for NullRenderer {
        fn render(
            &mut self,
            _snap: StateSnapshot<'_>,
            _frame: &crate::render::RenderedFrame,
        ) -> io::Result<()> {
            Ok(())
        }
    }

    pub fn test_state() -> State {
        State::with_config(Config {
            renderer: Box::new(NullRenderer),
            keycombo_timeout: Duration::from_hours(24),
        })
        .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::test_state;
    use super::*;

    #[test]
    fn render_does_not_panic_on_empty_buffer() {
        let mut s = test_state();
        s.render().unwrap();
    }

    #[test]
    fn split_then_close_returns_to_single_window() {
        let mut s = test_state();
        s.apply(&[Rc::new(Action::WindowSplit(SplitDir::Horizontal))])
            .unwrap();
        s.apply(&[Rc::new(Action::WindowClose)]).unwrap();
        s.render().unwrap();
    }

    #[test]
    fn default_precompute_produces_expected_slots() {
        // Confirms the bundled `default-style.lisp` loads cleanly and
        // produces a populated frame. We assert structural invariants only
        // (no slot errors, at least one gutter, the standard decorators,
        // both status sides have content). The exact shape is theme-defined.
        let mut s = test_state();
        let (frame, err) = s.precompute_frame();
        assert!(err.is_none(), "no slot errors expected: {err:?}");
        assert!(!frame.status_left.is_empty(), "expected left segments");
        assert!(!frame.status_right.is_empty(), "expected right segments");
        let bufno = s.windows.focused_bufno();
        let bf = &frame.per_buf[bufno];
        assert!(!bf.gutters.is_empty());
        assert!(bf.decorators.len() >= 3);
        // The bundled theme adds one bottom hint bar.
        assert!(!frame.bottom_extra.is_empty());
    }
}
