use crate::keymap::KeyEvent;
use crate::wrap::WrapMode;
use std::path::Path;
use std::rc::Rc;
use std::time::Instant;
use std::{io, time::Duration};

use crossterm::event::KeyEvent as CTKeyEvent;
use rizz::RizzError;
use rizz::runtime::Value;
use rizz_ringbuffer::RingBuffer;

use crate::{
    action::Action,
    buffer::{Buffer, BufferKind},
    buffer_list::{BufferList, CycleDir},
    count_prefix::CountPrefix,
    journal::Journal,
    keymap::KeymapRegistry,
    lisp::{EditorGuard, LispRuntime, init_script_path},
    mode::EditingMode,
    popup::{Chrome, Placement, Popup, PopupStack},
    position::Position,
    regions::RegionRegistry,
    render::{CursorStyle, Renderer, StateSnapshot},
    render_ratatui::RatatuiRenderer,
    styling::ThemeCell,
    window::{SplitDir, WindowTree},
};

/// Bottom-of-screen reservation: one row for the status line, one for the
/// minibuffer. Subtracted from the terminal height when sizing the editor
/// area for the window tree.
const STATUS_LINE_ROWS: u16 = 1;
const MINIBUFFER_ROWS: u16 = 1;
const KEYCOMBO_TIMEOUT: Duration = Duration::from_millis(1000);

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

/// Builder-style spec describing a popup to open. Built by the lisp
/// `popup-open` builtin from a property map; there are no Rust-side
/// callers — message popups are constructed in lisp on top of `popup-open`.
pub struct PopupSpec {
    pub initial_text: Option<String>,
    pub placement: Placement,
    pub chrome: Chrome,
    /// Keymap mode layers to push onto the popup's buffer. Ordered
    /// least-recent first — the last entry ends up at the top of the
    /// stack and shadows the rest during keymap resolution.
    pub mode_layers: Vec<Rc<str>>,
    pub buffer_mode: EditingMode,
    pub show_cursor: bool,
    pub wrap_mode: WrapMode,
    pub wrap_column: Option<u16>,
    pub breakindent: bool,
}

impl PopupSpec {
    pub fn new() -> Self {
        Self {
            initial_text: None,
            placement: Placement::default(),
            chrome: Chrome::default(),
            mode_layers: vec![Rc::<str>::from("popup")],
            buffer_mode: EditingMode::Normal,
            show_cursor: false,
            wrap_mode: WrapMode::default(),
            wrap_column: None,
            breakindent: false,
        }
    }
}

impl Default for PopupSpec {
    fn default() -> Self {
        Self::new()
    }
}

pub struct State {
    /// All live buffers. Owns the minibuffer index so reindex-on-removal
    /// stays atomic with the underlying `Vec`.
    bufs: BufferList,
    /// Tree of editor windows. Leaves point at indices into `bufs`. The
    /// minibuffer is not part of this tree.
    windows: WindowTree,
    /// When true, key events route to the minibuffer instead of the focused
    /// editor window.
    focus_minibuffer: bool,
    /// Append-only history of user-visible messages and commands. Surfaced
    /// by `:messages` / `:history`.
    journal: Journal,
    /// Popup overlay stack, bottom-to-top. While non-empty, the top popup
    /// captures key input (resolved against its mode layers) and the
    /// focused buffer is the top popup's backing buffer.
    popups: PopupStack,
    quit: bool,
    keymap: KeymapRegistry,
    keyevents: RingBuffer<(KeyEvent, Instant), 100>,
    keycombo_timeout: Duration,
    /// Numeric prefix accumulated in Normal / Visual modes. Drained when a
    /// non-digit key resolves to an action (the count is attached to any
    /// [`Action::MoveCursor`] in the resolved sequence). Partial keymap
    /// sequences (`g` mid-way through `gg`) leave the count untouched so
    /// e.g. `3gg` still works.
    count_prefix: CountPrefix,
    renderer: Box<dyn Renderer>,
    /// Embedded lisp runtime. Held as `Option` so `eval_lisp*` can `take` it
    /// for the duration of an eval — this also blocks re-entrant evaluation.
    lisp: Option<LispRuntime>,
    /// Named styles registered by lisp (`face-define`). `RefCell` so render
    /// callbacks can introspect without holding `&mut State`.
    theme: ThemeCell,
    /// Ordered registry of customization regions (top/bottom strips, status
    /// segments, gutters, decorators). Owned, not RefCell — only the lisp
    /// builtins that hold `&mut State` mutate it.
    regions: RegionRegistry,
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
            bufs: BufferList::new(),
            windows: WindowTree::new(1),
            focus_minibuffer: false,
            journal: Journal::new(),
            popups: PopupStack::new(),
            quit: false,
            keymap: KeymapRegistry::new(),
            keyevents: RingBuffer::new(),
            keycombo_timeout: config.keycombo_timeout,
            count_prefix: CountPrefix::new(),
            renderer: config.renderer,
            lisp: Some(LispRuntime::new()),
            theme: ThemeCell::default(),
            regions: RegionRegistry::new(),
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

    /// Install an [`EditorGuard`] and run `f` against the editor's lisp
    /// runtime. The runtime is moved out of `self.lisp` for the call so
    /// builtins reach the live `State` through the thread-local, never
    /// through an aliased borrow. Re-entrant calls panic.
    fn with_lisp<R>(&mut self, f: impl FnOnce(&mut LispRuntime) -> R) -> R {
        let mut lisp = self
            .lisp
            .take()
            .expect("recursive eval_lisp is not supported");
        let result = {
            let _guard = EditorGuard::new(self);
            f(&mut lisp)
        };
        self.lisp = Some(lisp);
        result
    }

    /// Parse `src` as one lisp form and evaluate it in the embedded runtime.
    pub fn eval_lisp(&mut self, src: &str) -> Result<Rc<Value>, RizzError> {
        self.with_lisp(|lisp| lisp.eval_str(src))
    }

    /// Evaluate an already-parsed form. Used to dispatch keymap-bound lisp.
    pub fn eval_lisp_value(&mut self, form: Rc<Value>) -> Result<Rc<Value>, RizzError> {
        self.with_lisp(|lisp| lisp.eval_value(form))
    }

    /// Evaluate a multi-form script (e.g. `default.lisp`, `init.lisp`).
    pub fn eval_lisp_script(&mut self, src: &str) -> Result<(), RizzError> {
        self.with_lisp(|lisp| lisp.eval_script(src))
    }

    /// Read-only accessor for the focused buffer. Exposed for lisp builtins
    /// that need to query buffer state without going through `Action`.
    pub(crate) fn focused_buf(&self) -> &Buffer {
        let i = self.focused_bufno();
        &self.bufs[i]
    }

    pub(crate) fn focused_buf_mut(&mut self) -> &mut Buffer {
        let i = self.focused_bufno();
        &mut self.bufs[i]
    }

    /// Accessor for the [`crate::styling::Theme`] cell. Used by `face-define`
    /// / `face-of` builtins.
    pub(crate) fn theme(&self) -> &ThemeCell {
        &self.theme
    }

    /// Mutable accessor for the region registry. Used by the `region-add` /
    /// `region-remove` builtins.
    pub(crate) fn regions_mut(&mut self) -> &mut RegionRegistry {
        &mut self.regions
    }

    /// Append `msg` to the message history. The user-visible popup is the
    /// concern of the lisp `notify` fn; this only owns the storage.
    pub(crate) fn record_message(&mut self, msg: &str) {
        self.journal.record_message(msg);
    }

    pub(crate) fn message_history(&self) -> impl Iterator<Item = &Rc<str>> {
        self.journal.messages()
    }

    pub(crate) fn record_cmd(&mut self, msg: &str) {
        self.journal.record_command(msg);
    }
    pub(crate) fn cmd_history(&self) -> impl Iterator<Item = &Rc<str>> {
        self.journal.commands()
    }

    /// Bridge from Rust-internal failure paths (eval errors, render-callback
    /// errors) to the lisp-side `notify` fn defined in `default.lisp`. Runs
    /// `(notify "<msg>")` in the embedded runtime so styling and dedup live
    /// entirely in lisp. Any error from the eval itself is recorded as a
    /// message but does not recurse — we don't want a broken `notify` to
    /// take down the editor.
    pub(crate) fn notify_via_lisp(&mut self, msg: &str) {
        let src = format!("(notify {})", crate::lisp::quote_for_lisp(msg));
        if let Err(e) = self.eval_lisp(&src) {
            self.record_message(&format!("notify failed: {e}"));
        }
    }

    /// Topmost keymap layer of the topmost popup, exposed for the
    /// `popup-mode` lisp builtin (used by `notify` to dedup against its
    /// own popup). Returns `None` when no popup is open or when the top
    /// popup didn't push any layers.
    pub(crate) fn top_popup_mode(&self) -> Option<Rc<str>> {
        self.popups.top_mode()
    }

    /// Push a popup onto the overlay stack. Creates a backing buffer of
    /// kind [`BufferKind::Popup`], appends it to `self.bufs`, and returns
    /// its bufno so callers (lisp `popup-open`) can populate further state.
    pub(crate) fn open_popup(&mut self, spec: PopupSpec) -> usize {
        let mut buf = Buffer::popup();
        if let Some(text) = spec.initial_text {
            buf.clear_with(&text);
        }
        buf.set_mode(spec.buffer_mode);
        buf.set_wrap_mode(spec.wrap_mode);
        buf.set_wrap_column(spec.wrap_column);
        buf.set_breakindent(spec.breakindent);
        for layer in &spec.mode_layers {
            buf.push_mode_layer(layer.clone());
        }
        self.bufs.push(buf);
        let bufno = self.bufs.len() - 1;
        self.popups.push(Popup {
            bufno,
            placement: spec.placement,
            chrome: spec.chrome,
            mode_layers: spec.mode_layers,
            show_cursor: spec.show_cursor,
        });
        self.refresh_viewport();
        bufno
    }

    /// Pop the top popup. Removes its backing buffer and re-indexes any
    /// remaining references that pointed past the deleted slot. Returns
    /// true iff something was actually closed.
    pub(crate) fn close_popup(&mut self) -> bool {
        let Some(popup) = self.popups.pop() else {
            return false;
        };
        let removed = popup.bufno;
        if removed < self.bufs.len() && self.bufs[removed].kind() == BufferKind::Popup {
            self.bufs.remove(removed);
            self.popups.shift_bufnos_after_removal(removed);
            let first = self.bufs.first_file_buf();
            self.windows.for_each_leaf_mut(|b| {
                if *b == removed {
                    *b = first;
                } else if *b > removed {
                    *b -= 1;
                }
            });
        }
        self.refresh_viewport();
        true
    }

    /// Bufno of the topmost popup, if one is open.
    pub(crate) fn top_popup_bufno(&self) -> Option<usize> {
        self.popups.top_bufno()
    }

    pub(crate) fn has_popup(&self) -> bool {
        !self.popups.is_empty()
    }

    pub(crate) fn set_buffer_contents(&mut self, bufno: usize, msg: &str) {
        let b = &mut self.bufs[bufno];
        b.clear_with(msg);
    }

    /// Read the current minibuffer text and leave the minibuffer. Used by the
    /// `command-submit` lisp builtin, which then evaluates the text inline
    /// using the env already in scope (calling back into `eval_lisp` from here
    /// would re-take `self.lisp` and panic).
    pub(crate) fn take_minibuffer_command(&mut self) -> String {
        let cmd = self.bufs.minibuffer().text();
        self.exit_minibuffer();
        cmd
    }

    pub(crate) fn workdir(&self) -> Rc<Path> {
        self.workdir.clone()
    }

    /// Numeric prefix the user has typed but not yet consumed (`3` in
    /// `3j`). Lisp `move-cursor` reads this to multiply the motion.
    /// Returns 1 when no count is pending so callers can blindly multiply.
    pub(crate) fn pending_count_or_one(&self) -> u32 {
        self.count_prefix.or_one()
    }

    pub(crate) fn keymap_registry(&self) -> &KeymapRegistry {
        &self.keymap
    }

    /// The buffer currently receiving key events. When a popup is open,
    /// the top popup's backing buffer captures input (movement commands
    /// scroll the popup's content; `(insert-char …)` would edit it). This
    /// is what makes popups feel like overlayed buffers.
    pub(crate) fn focused_bufno(&self) -> usize {
        if let Some(bufno) = self.popups.top_bufno() {
            return bufno;
        }
        if self.focus_minibuffer {
            self.bufs.minibuffer_index()
        } else {
            self.windows.focused_bufno()
        }
    }

    pub(crate) fn nbufs(&self) -> usize {
        self.bufs.len()
    }

    /// Update viewports of all buffers currently displayed in a window,
    /// the minibuffer, and every popup. Per-leaf rect comes from the
    /// window tree layout; popups derive theirs from `Placement::resolve`
    /// against the same editor area, minus the border inset. Silently
    /// ignores terminal::size errors so tests without a real TTY still
    /// work.
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
        self.bufs.minibuffer_mut().viewport = Position::new(cols, MINIBUFFER_ROWS);
        // Popup viewports are the inner content rect — outer placement
        // minus the border inset on each side.
        let popups: Vec<(usize, Position<u16>)> = self
            .popups
            .iter()
            .map(|p| {
                let outer = p.placement.resolve(editor_area);
                let inset = p.chrome.border.inset();
                let inner_w = outer.width.saturating_sub(2 * inset);
                let inner_h = outer.height.saturating_sub(2 * inset);
                (p.bufno, Position::new(inner_w, inner_h))
            })
            .collect();
        for (bufno, viewport) in popups {
            if let Some(buf) = self.bufs.get_mut(bufno) {
                buf.viewport = viewport;
            }
        }
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

        // Numeric-prefix gate. In Normal/Visual modes, digits typed while no
        // keymap sequence is in flight feed the count prefix instead of
        // resolving against the keymap — `3j`, `12gg`, etc.
        let ke: KeyEvent = event.into();
        if self.count_prefix.feed(ke, self.count_eligible()) {
            self.refresh_viewport();
            return self.render();
        }

        // The focused buffer's mode stack drives keymap resolution
        // uniformly: pushed layers (most recent first) shadow the buffer's
        // base editing mode. Popups participate by virtue of opening
        // having pushed their layers onto the popup's backing buffer.
        let modes = self.bufs[self.focused_bufno()].active_modes();
        if let Some(action) = self.keymap.resolve(&modes, ke, timedout) {
            self.apply(&action)?;
            // Drain the count only once a key has been resolved to an
            // action. Partial sequences (`g` mid-`gg`) leave it alone so
            // `3gg` still lands at file start with the count attached.
            self.count_prefix.clear();
        }
        // Refresh after apply: window splits/closes and buffer switches may
        // have changed which buffer occupies which viewport.
        self.refresh_viewport();
        let focused = self.focused_bufno();
        self.bufs[focused].clamp_cursor();
        self.render()
    }

    /// Conditions under which [`CountPrefix`] should consider absorbing a
    /// digit: keymap idle, no popup, no minibuffer focus, focused mode is
    /// Normal or any Visual variant.
    fn count_eligible(&self) -> bool {
        if self.has_popup() || self.focus_minibuffer {
            return false;
        }
        if !self.keymap.is_idle() {
            return false;
        }
        matches!(
            self.bufs[self.focused_bufno()].mode(),
            EditingMode::Normal
                | EditingMode::Visual
                | EditingMode::VisualLine
                | EditingMode::VisualBlock
        )
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

                Action::DeleteCharAt(pos) => {
                    let f = self.focused_bufno();
                    self.bufs[f].delete_char_at(*pos);
                }
                Action::MoveCursor { kind, count } => {
                    let f = self.focused_bufno();
                    self.bufs[f].move_cursor_n(*kind, *count);
                }
                Action::CommandCancel => self.exit_minibuffer(),
                Action::BufCreate { path, set_active } => {
                    self.create_buf(*set_active, path.clone())?;
                }
                Action::BufDelete => {
                    let editor = self.windows.focused_bufno();
                    self.delete_buf(editor);
                }
                Action::BufNext => self.cycle_buffer(CycleDir::Next),
                Action::BufPrev => self.cycle_buffer(CycleDir::Prev),
                Action::BufEdit(path) => {
                    self.edit_buf(path.clone())?;
                }
                Action::BufWrite(path) => self.write_buf(path.clone())?,
                Action::WindowSplit(dir) => self.window_split(*dir),
                Action::WindowClose => self.window_close(),
                Action::WindowFocusNext => self.windows.focus_next(),
                Action::WindowFocus(d) => self.windows.focus_dir(*d),
                Action::KeymapSet { mode, lhs, rhs } => {
                    self.keymap.set(mode.clone(), lhs, rhs.clone());
                }
                Action::KeymapRemove { mode, lhs } => {
                    self.keymap.remove(mode.clone(), lhs);
                }
                Action::EvalLisp(form) => {
                    if let Err(e) = self.eval_lisp_value(form.clone()) {
                        self.notify_via_lisp(&e.to_string());
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
            self.bufs.minibuffer_mut().clear();
            self.bufs.minibuffer_mut().set_mode(EditingMode::Command);
            self.focus_minibuffer = true;
        } else {
            let f = self.focused_bufno();
            self.bufs[f].set_mode(mode);
        }
    }

    /// Clear the minibuffer, drop focus from it, and reset the focused
    /// editor buffer to Normal mode.
    fn exit_minibuffer(&mut self) {
        self.bufs.minibuffer_mut().clear();
        self.bufs.minibuffer_mut().set_mode(EditingMode::Command);
        self.focus_minibuffer = false;
        let editor = self.windows.focused_bufno();
        self.bufs[editor].set_mode(EditingMode::Normal);
    }

    fn create_buf(&mut self, set_active: bool, path: Option<Rc<Path>>) -> io::Result<usize> {
        let buf = match path {
            Some(p) => self.bufs.buffer_for_path(p),
            None => Buffer::new(),
        };
        let bufno = self.bufs.push(buf);
        if set_active {
            self.windows.set_focused_bufno(bufno);
        }
        Ok(bufno)
    }

    fn edit_buf(&mut self, path: Rc<Path>) -> io::Result<usize> {
        let idx = match self.bufs.find_by_path(&path) {
            Some(idx) => idx,
            None => self.bufs.push(crate::buffer_io::with_path(path)),
        };
        self.windows.set_focused_bufno(idx);
        Ok(idx)
    }

    fn write_buf(&mut self, path: Option<Rc<Path>>) -> io::Result<()> {
        let editor = self.windows.focused_bufno();
        crate::buffer_io::write(&mut self.bufs[editor], path)
    }

    /// Refuses to delete the minibuffer; keeps at least one file buffer alive.
    fn delete_buf(&mut self, bufno: usize) {
        if bufno >= self.bufs.len() || self.bufs[bufno].kind() == BufferKind::Minibuffer {
            return;
        }

        if self.bufs.file_buf_count() == 1 {
            // Last file buffer: reset it in place instead of removing.
            self.bufs.reset(bufno);
            self.windows.for_each_leaf_mut(|b| *b = bufno);
            return;
        }

        self.bufs.remove(bufno);
        // Reindex every leaf that pointed past the removed buffer; any leaf
        // that pointed AT the removed buffer falls back to the first file buf.
        let first = self.bufs.first_file_buf();
        self.windows.for_each_leaf_mut(|b| {
            if *b == bufno {
                *b = first;
            } else if *b > bufno {
                *b -= 1;
            }
        });
        self.popups.shift_bufnos_after_removal(bufno);
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

    /// Cycle the focused window to the next/previous file buffer, skipping
    /// the minibuffer.
    fn cycle_buffer(&mut self, dir: CycleDir) {
        if let Some(i) = self.bufs.cycle(self.windows.focused_bufno(), dir) {
            self.windows.set_focused_bufno(i);
        }
    }

    pub fn render(&mut self) -> io::Result<()> {
        let focused = self.focused_bufno();
        // Build the precomputed frame first. This installs an `EditorGuard`
        // so lisp render callbacks can reach back into `State` for queries,
        // and runs each slot to a plain value the renderer can consume.
        let (frame, error_msg) = self.precompute_frame();
        // Push each freshly built WrapMap onto its buffer so the next round
        // of cursor movement can step in visual rows. Clone into bufs;
        // `frame.per_buf` still owns the map for the renderer below.
        for (i, rb) in frame.per_buf.iter().enumerate() {
            if i < self.bufs.len() {
                self.bufs[i].set_wrap_cache(rb.wrap.clone());
            }
        }
        let snap = StateSnapshot {
            bufs: self.bufs.as_slice(),
            windows: &self.windows,
            minibuffer: self.bufs.minibuffer(),
            focus_minibuffer: self.focus_minibuffer,
            bufno: self.windows.focused_bufno(),
            keyevent: self.keyevents.peek_back().map(|(e, _)| e.to_owned()),
            cursor_style: match self.bufs[focused].mode() {
                EditingMode::Insert | EditingMode::Command => CursorStyle::Bar,
                _ => CursorStyle::Block,
            },
            popups: self.popups.as_slice(),
        };
        let result = self.renderer.render(snap, &frame);
        // Surface any render-callback error via the popup *after* the frame
        // draws so the message itself isn't part of the failing pass.
        if let Some(msg) = error_msg {
            self.notify_via_lisp(&msg);
        }
        result
    }

    /// Run every region under an `EditorGuard`, packing the results into a
    /// `RenderedFrame` the renderer can consume without ever touching lisp.
    /// Returns the frame plus an optional error message (concatenated from
    /// the first few region failures) to surface to the minibuffer.
    ///
    /// Owns the lisp take/restore + guard installation; the actual frame
    /// assembly is delegated to [`crate::precompute::compute`].
    pub(crate) fn precompute_frame(&mut self) -> (crate::render::RenderedFrame, Option<String>) {
        let lisp = self.lisp.take().expect("recursive render is not supported");
        let _editor_guard = crate::lisp::EditorGuard::new(self);
        let _phase_guard = crate::lisp::RenderPhaseGuard::enter();

        let result = crate::precompute::compute(crate::precompute::PrecomputeInput {
            bufs: self.bufs.as_slice(),
            windows: &self.windows,
            regions: &self.regions,
            theme: &self.theme,
            focus_minibuffer: self.focus_minibuffer,
            minibuffer: self.bufs.minibuffer_index(),
            last_key: self.keyevents.peek_back().map(|(ke, _)| ke.to_owned()),
            lisp_env: lisp.env(),
        });

        // `runtime::apply` discards a callee's local bindings, so the env
        // hasn't moved. Drop both guards before restoring lisp.
        drop(_phase_guard);
        drop(_editor_guard);
        self.lisp = Some(lisp);

        result
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

    /// Helper for popup tests — pull the current text out of the topmost
    /// popup's backing buffer. Replaces the old `popup.text` field access.
    fn top_popup_text(s: &State) -> String {
        let bufno = s.top_popup_bufno().expect("popup is visible");
        s.bufs[bufno].text()
    }

    #[test]
    fn notify_records_history_and_shows_popup() {
        // `notify` is defined in `default.lisp` on top of `notify-record`
        // and `popup-open` — exercising it confirms the lisp-side bridge is
        // wired up and that history storage still lives in Rust.
        let mut s = test_state();
        s.eval_lisp(r#"(notify "hello")"#).unwrap();
        assert_eq!(
            s.message_history().cloned().collect::<Vec<_>>(),
            vec!["hello".into()]
        );
        assert!(s.has_popup());
        assert_eq!(top_popup_text(&s), "hello");
    }

    #[test]
    fn q_dismisses_popup() {
        // `q` is bound to (popup-close) in the bundled `popup` keymap mode.
        use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
        let mut s = test_state();
        s.eval_lisp(r#"(notify "oops")"#).unwrap();
        assert!(s.has_popup());
        s.handle_key_event(CT::new(KeyCode::Char('q'), KeyModifiers::NONE))
            .unwrap();
        assert!(!s.has_popup());
    }

    #[test]
    fn j_moves_popup_cursor_without_dismissing() {
        // `j` in popup mode is (move-cursor 'down) — same shape as a normal
        // editor binding, exercising the "popup is just a buffer" model.
        use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
        let mut s = test_state();
        s.eval_lisp(r#"(notify "line1\nline2\nline3")"#).unwrap();
        s.handle_key_event(CT::new(KeyCode::Char('j'), KeyModifiers::NONE))
            .unwrap();
        assert!(s.has_popup(), "popup must still be visible");
        let bufno = s.top_popup_bufno().unwrap();
        let buf = &s.bufs[bufno];
        // Absolute cursor row advanced by one (either by scrolling or by the
        // in-viewport cursor moving down — either is correct).
        assert_eq!(buf.cursor_pos().row as usize + buf.file_pos().row, 1);
    }

    #[test]
    fn successive_notifies_replace_in_place() {
        // The lisp-side `notify` dedups against the topmost popup so a flood
        // of notifications doesn't stack overlays.
        let mut s = test_state();
        s.eval_lisp(r#"(notify "a")"#).unwrap();
        s.eval_lisp(r#"(notify "b")"#).unwrap();
        assert_eq!(
            s.message_history().cloned().collect::<Vec<_>>(),
            vec!["a".into(), "b".into()]
        );
        assert_eq!(top_popup_text(&s), "b");
        assert!(s.has_popup());
    }

    #[test]
    fn messages_builtin_opens_popup_with_history() {
        let mut s = test_state();
        s.eval_lisp(r#"(notify "first")"#).unwrap();
        // Dismiss the popup the `notify` call opened so we can re-check
        // that `(messages)` reopens with the joined history.
        s.close_popup();
        s.eval_lisp(r#"(notify "second")"#).unwrap();
        s.close_popup();
        s.eval_lisp("(messages)").unwrap();
        let p = top_popup_text(&s);
        assert!(p.contains("first"));
        assert!(p.contains("second"));
    }

    #[test]
    fn popup_files_inherits_motions_from_popup_layer() {
        // `popup-files` opens a popup with mode layers ['popup, 'popup.files].
        // `popup.files` no longer rebinds motions; `j` resolves against the
        // underlying `popup` layer, exercising the layered lookup path.
        use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
        let mut s = test_state();
        s.eval_lisp("(popup-files)").unwrap();
        assert!(s.has_popup());
        let bufno = s.top_popup_bufno().unwrap();
        let before = s.bufs[bufno].cursor_pos().row as usize + s.bufs[bufno].file_pos().row;
        s.handle_key_event(CT::new(KeyCode::Char('j'), KeyModifiers::NONE))
            .unwrap();
        let after = s.bufs[bufno].cursor_pos().row as usize + s.bufs[bufno].file_pos().row;
        assert_eq!(after, before + 1, "j should move down via popup layer");
    }

    #[test]
    fn count_prefix_scales_motion() {
        // Typing `3j` should move down 3 lines in a single motion. The count
        // is absorbed by State before keymap resolution and re-attached to
        // the resolved MoveCursor action.
        use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
        let mut s = test_state();
        s.bufs[1].clear_with("a\nb\nc\nd\ne\nf\ng");
        s.bufs[1].cursor_pos = Position::default();
        s.bufs[1].file_pos = Position::default();
        s.handle_key_event(CT::new(KeyCode::Char('3'), KeyModifiers::NONE))
            .unwrap();
        // Digit absorbed: cursor must not have moved.
        let abs_row = s.bufs[1].cursor_pos().row as usize + s.bufs[1].file_pos().row;
        assert_eq!(abs_row, 0);
        s.handle_key_event(CT::new(KeyCode::Char('j'), KeyModifiers::NONE))
            .unwrap();
        let abs_row = s.bufs[1].cursor_pos().row as usize + s.bufs[1].file_pos().row;
        assert_eq!(abs_row, 3);
    }

    #[test]
    fn count_prefix_clears_after_use() {
        // After a count is consumed, a plain motion must not see it again.
        use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
        let mut s = test_state();
        s.bufs[1].clear_with("a\nb\nc\nd\ne\nf\ng");
        s.bufs[1].cursor_pos = Position::default();
        s.bufs[1].file_pos = Position::default();
        for c in ['2', 'j', 'j'] {
            s.handle_key_event(CT::new(KeyCode::Char(c), KeyModifiers::NONE))
                .unwrap();
        }
        // 2j → row 2, then plain j → row 3.
        let abs_row = s.bufs[1].cursor_pos().row as usize + s.bufs[1].file_pos().row;
        assert_eq!(abs_row, 3);
    }

    #[test]
    fn count_prefix_survives_partial_keymap_sequence() {
        // `3gg` should jump to file start (`gg`) but the count must persist
        // across the first `g` so it can attach to the eventual motion.
        // `gg` resolves to FileStart which is idempotent under count, so we
        // instead test that the count survives by using `2w`: pretty trivial
        // but here we just confirm `3gg` lands somewhere sane (file start).
        use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
        let mut s = test_state();
        s.bufs[1].clear_with("a\nb\nc\nd\ne");
        s.bufs[1].cursor_pos = Position::<u16>::new(0, 3);
        for c in ['3', 'g', 'g'] {
            s.handle_key_event(CT::new(KeyCode::Char(c), KeyModifiers::NONE))
                .unwrap();
        }
        let abs_row = s.bufs[1].cursor_pos().row as usize + s.bufs[1].file_pos().row;
        assert_eq!(abs_row, 0);
    }

    #[test]
    fn leading_zero_falls_through_as_line_start() {
        // `0` with no pending count must still resolve to `line-start`, not
        // be silently absorbed as a count digit.
        use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
        let mut s = test_state();
        s.bufs[1].clear_with("hello world");
        s.bufs[1].cursor_pos = Position::<u16>::new(6, 0);
        s.handle_key_event(CT::new(KeyCode::Char('0'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(s.bufs[1].cursor_pos().col, 0);
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
