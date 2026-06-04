use crate::keymap::KeyEvent;
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
    keymap::KeymapRegistry,
    lisp::{EditorGuard, LispRuntime, init_script_path},
    mode::EditingMode,
    popup::{Chrome, Placement, Popup},
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

/// Builder-style spec describing a popup to open. The lisp `popup-open`
/// builtin builds one of these from a property map; internal callers
/// (`push_message`, `show_all_messages`) construct one directly.
pub struct PopupSpec {
    pub initial_text: Option<String>,
    pub placement: Placement,
    pub chrome: Chrome,
    pub keymap_mode: Rc<str>,
    pub buffer_mode: EditingMode,
    pub show_cursor: bool,
}

impl PopupSpec {
    pub fn new() -> Self {
        Self {
            initial_text: None,
            placement: Placement::default(),
            chrome: Chrome::default(),
            keymap_mode: Rc::<str>::from("popup"),
            buffer_mode: EditingMode::Normal,
            show_cursor: false,
        }
    }

    /// Preset matching the legacy message popup: centered with a plain
    /// border and the canonical message title.
    pub fn message(text: &str) -> Self {
        let mut spec = Self::new();
        spec.initial_text = Some(text.to_string());
        spec.chrome.title = Some(Rc::<str>::from("message — any key to dismiss"));
        spec
    }
}

impl Default for PopupSpec {
    fn default() -> Self {
        Self::new()
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
    /// Append-only history of every user-visible message. Surfaced by
    /// `:messages`.
    messages: Vec<Rc<str>>,
    /// Popup overlay stack, bottom-to-top. While non-empty, the top popup
    /// captures key input (resolved against its `keymap_mode`) and the
    /// focused buffer is the top popup's backing buffer.
    popups: Vec<Popup>,
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
            bufs: vec![Buffer::minibuffer(), Buffer::new()],
            windows: WindowTree::new(1),
            focus_minibuffer: false,
            minibuffer: 0,
            messages: Vec::new(),
            popups: Vec::new(),
            quit: false,
            keymap: KeymapRegistry::new(),
            keyevents: RingBuffer::new(),
            keycombo_timeout: config.keycombo_timeout,
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

    /// Record `msg` in the message history and surface it to the user as a
    /// modal popup. Sink for `(message ...)`, eval errors, render-callback
    /// errors, and command-submit results.
    ///
    /// If the topmost popup is already a message popup we replace it in
    /// place instead of stacking — that mirrors the pre-generalization
    /// behavior where successive messages overwrote each other.
    pub(crate) fn push_message(&mut self, msg: &str) {
        self.messages.push(msg.into());
        self.replace_or_open_message_popup(PopupSpec::message(msg));
    }

    /// Open the popup with the full message history (newline-separated).
    /// Invoked by `:messages`.
    pub(crate) fn show_all_messages(&mut self) {
        let text = self.messages.join("\n");
        self.replace_or_open_message_popup(PopupSpec::message(&text));
    }

    pub(crate) fn message_history(&mut self) -> &[Rc<str>] {
        &self.messages
    }

    /// If the top popup is a message popup (default keymap mode), refill
    /// its buffer and reset the cursor instead of stacking another one.
    /// Otherwise open a fresh popup.
    fn replace_or_open_message_popup(&mut self, spec: PopupSpec) {
        if let Some(top) = self.popups.last()
            && top.keymap_mode.as_ref() == "popup"
        {
            let bufno = top.bufno;
            if let Some(text) = &spec.initial_text {
                self.bufs[bufno].clear_with(text);
            } else {
                self.bufs[bufno].clear();
            }
            return;
        }
        self.open_popup(spec);
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
        self.bufs.push(buf);
        let bufno = self.bufs.len() - 1;
        self.popups.push(Popup {
            bufno,
            placement: spec.placement,
            chrome: spec.chrome,
            keymap_mode: spec.keymap_mode,
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
            if self.minibuffer > removed {
                self.minibuffer -= 1;
            }
            for p in &mut self.popups {
                if p.bufno > removed {
                    p.bufno -= 1;
                }
            }
            let first = self.first_file_buf();
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
        self.popups.last().map(|p| p.bufno)
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
        let cmd = self.bufs[self.minibuffer].text();
        self.exit_minibuffer();
        cmd
    }

    pub(crate) fn workdir(&self) -> Rc<Path> {
        self.workdir.clone()
    }

    pub(crate) fn keymap_registry(&self) -> &KeymapRegistry {
        &self.keymap
    }

    /// The buffer currently receiving key events. When a popup is open,
    /// the top popup's backing buffer captures input (movement commands
    /// scroll the popup's content; `(insert-char …)` would edit it). This
    /// is what makes popups feel like overlayed buffers.
    pub(crate) fn focused_bufno(&self) -> usize {
        if let Some(p) = self.popups.last() {
            return p.bufno;
        }
        if self.focus_minibuffer {
            self.minibuffer
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
        self.bufs[self.minibuffer].viewport = Position::new(cols, MINIBUFFER_ROWS);
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

        // When a popup is on top of the stack the keymap is resolved against
        // its `keymap_mode` (default `"popup"`), not the focused buffer's
        // editing mode. That decouples popup interaction from the underlying
        // buffer mode — a popup buffer may still be in Normal/Insert, but
        // pop-up-specific bindings (j/k scroll, q dismiss, …) live in their
        // own mode keymap.
        let mode_name: Rc<str> = if let Some(p) = self.popups.last() {
            p.keymap_mode.clone()
        } else {
            self.bufs[self.focused_bufno()].mode().as_str().into()
        };
        if let Some(action) = self.keymap.resolve(mode_name, event.into(), timedout) {
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

                Action::DeleteCharAt(pos) => {
                    let f = self.focused_bufno();
                    self.bufs[f].delete_char_at(*pos);
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
                    self.keymap.set(mode.clone(), lhs, rhs.clone());
                }
                Action::KeymapRemove { mode, lhs } => {
                    self.keymap.remove(mode.clone(), lhs);
                }
                Action::EvalLisp(form) => {
                    if let Err(e) = self.eval_lisp_value(form.clone()) {
                        self.push_message(&e.to_string());
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
            popups: &self.popups,
        };
        let result = self.renderer.render(snap, &frame);
        // Surface any render-callback error via the popup *after* the frame
        // draws so the message itself isn't part of the failing pass.
        if let Some(msg) = error_msg {
            self.push_message(&msg);
        }
        result
    }

    /// Run every region under an `EditorGuard`, packing the results into a
    /// `RenderedFrame` the renderer can consume without ever touching lisp.
    /// Returns the frame plus an optional error message (concatenated from
    /// the first few region failures) to surface to the minibuffer.
    pub(crate) fn precompute_frame(&mut self) -> (crate::render::RenderedFrame, Option<String>) {
        use crate::regions::{
            RegionAnchor, produce_decorator, produce_gutter, produce_status_span,
            produce_strip_rows,
        };
        use crate::render::{RenderedBuffer, RenderedFrame, RenderedStrip};

        let lisp = self.lisp.take().expect("recursive render is not supported");
        let _editor_guard = crate::lisp::EditorGuard::new(self);
        let _phase_guard = crate::lisp::RenderPhaseGuard::enter();

        // Snapshot the theme so callbacks that mutate it (`face-define`) only
        // affect the next frame.
        let theme = self.theme.borrow().clone();
        let default_style = theme.resolve("default").unwrap_or_default();
        let env = lisp.env().clone();

        let mut error_chunks: Vec<String> = Vec::new();
        let record = |chunks: &mut Vec<String>, region_name: &str, err: rizz::RizzError| {
            if chunks.len() < 3 {
                chunks.push(format!("[{region_name}] {err}"));
            }
        };

        // Read-only snapshot status/strip producers consume.
        let snap = StateSnapshot {
            bufs: &self.bufs,
            windows: &self.windows,
            minibuffer: &self.bufs[self.minibuffer],
            focus_minibuffer: self.focus_minibuffer,
            bufno: self.windows.focused_bufno(),
            keyevent: self.keyevents.peek_back().map(|(ke, _)| ke.to_owned()),
            cursor_style: CursorStyle::Block,
            popups: &[],
        };

        // One pass over the registry, routing each region's output to the
        // matching `RenderedFrame` bucket.
        let mut top_extra: Vec<RenderedStrip> = Vec::new();
        let mut bottom_extra: Vec<RenderedStrip> = Vec::new();
        let mut status_left: Vec<ratatui::text::Span<'static>> = Vec::new();
        let mut status_right: Vec<ratatui::text::Span<'static>> = Vec::new();
        for region in self.regions.iter() {
            match region.anchor {
                RegionAnchor::Top => match produce_strip_rows(region, &theme, &env) {
                    Ok(lines) => top_extra.push(RenderedStrip { lines }),
                    Err(e) => record(&mut error_chunks, &region.name, e),
                },
                RegionAnchor::Bottom => match produce_strip_rows(region, &theme, &env) {
                    Ok(lines) => bottom_extra.push(RenderedStrip { lines }),
                    Err(e) => record(&mut error_chunks, &region.name, e),
                },
                RegionAnchor::StatusLeft => {
                    match produce_status_span(region, &snap, &theme, &env) {
                        Ok(spans) => status_left.extend(spans),
                        Err(e) => record(&mut error_chunks, &region.name, e),
                    }
                }
                RegionAnchor::StatusRight => {
                    match produce_status_span(region, &snap, &theme, &env) {
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
        let mut per_buf = Vec::with_capacity(self.bufs.len());
        for (i, buf) in self.bufs.iter().enumerate() {
            let mut rb = RenderedBuffer::default();
            let participates_in_regions =
                i != self.minibuffer && buf.kind() != crate::buffer::BufferKind::Popup;
            if participates_in_regions {
                for region in self.regions.iter() {
                    match region.anchor {
                        RegionAnchor::Gutter { .. } => {
                            match produce_gutter(region, buf, &theme, &env) {
                                Ok(g) => rb.gutters.push(g),
                                Err(e) => record(&mut error_chunks, &region.name, e),
                            }
                        }
                        RegionAnchor::Decorator => {
                            match produce_decorator(region, buf, &theme, &env) {
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
            if i != self.minibuffer {
                let prop_ranges = crate::props::build_prop_ranges(buf, &theme);
                if !prop_ranges.ranges.is_empty() {
                    rb.decorators.push(prop_ranges);
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
                default_style,
                theme,
                top_extra,
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

    /// Helper for popup tests — pull the current text out of the topmost
    /// popup's backing buffer. Replaces the old `popup.text` field access.
    fn top_popup_text(s: &State) -> String {
        let bufno = s.top_popup_bufno().expect("popup is visible");
        s.bufs[bufno].text()
    }

    #[test]
    fn message_pushes_history_and_shows_popup() {
        let mut s = test_state();
        s.push_message("hello");
        assert_eq!(s.messages, vec!["hello".into()]);
        assert!(s.has_popup());
        assert_eq!(top_popup_text(&s), "hello");
    }

    #[test]
    fn q_dismisses_popup() {
        // `q` is bound to (popup-close) in the bundled `popup` keymap mode.
        use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
        let mut s = test_state();
        s.push_message("oops");
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
        s.push_message("line1\nline2\nline3");
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
    fn show_all_messages_joins_history() {
        let mut s = test_state();
        s.push_message("a");
        s.push_message("b");
        // Showing the latest message popped open after each push. Now
        // request the full history.
        s.show_all_messages();
        assert_eq!(top_popup_text(&s), "a\nb");
    }

    #[test]
    fn messages_builtin_opens_popup_with_history() {
        let mut s = test_state();
        s.eval_lisp(r#"(notify "first")"#).unwrap();
        // Dismiss the popup the `message` call opened so we can re-check
        // that `(messages)` reopens with the joined history.
        s.close_popup();
        s.eval_lisp(r#"(notify "second")"#).unwrap();
        s.close_popup();
        s.eval_lisp("(messages)").unwrap();
        assert_eq!(top_popup_text(&s), "first\nsecond");
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
