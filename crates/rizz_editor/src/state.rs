//! The editor process — single owner of every long-lived editor field.
//!
//! `State::apply` is the only mutator: every key event, lisp call, or
//! external trigger ultimately produces an [`rizz_actions::Action`] list and
//! sends it through here. The single-funnel invariant is what makes undo,
//! scripting, and tests tractable.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;
use std::{io, time::Duration};

use crossterm::event::KeyEvent as CTKeyEvent;
use rizz::RizzError;
use rizz::runtime::Value;
use rizz_ringbuffer::RingBuffer;

use rizz_actions::{Action, KeymapRegistry};
use rizz_core::{EditingMode, Position};
use rizz_input::{CountPrefix, KeyEvent};
use rizz_text::{Buffer, BufferKind, WrapMode, io as buffer_io};
use rizz_ts::TsRegistry;

use rizz_ui::{
    RatatuiRenderer, Renderer, StateSnapshot, ThemeCell, Widget, WindowTree,
    popup::{Placement, Popup, PopupStack},
    precompute,
    render::{CursorStyle, RenderedFrame},
};

use crate::buffer_list::{BufferList, CycleDir};
use crate::journal::Journal;
use crate::lisp::{EditorGuard, LispRuntime, RenderPhaseGuard};

pub use rizz_core::{FocusDir, SplitDir};

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
    pub edit_path: Option<PathBuf>,
    /// Directory holding `init.rz`. `None` selects the default for the build
    /// (the workspace root in debug/test, `$XDG_CONFIG_HOME/rizz` in release).
    pub config_dir: Option<PathBuf>,
}

impl Config {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            renderer: Box::new(RatatuiRenderer::new()?),
            keycombo_timeout: KEYCOMBO_TIMEOUT,
            edit_path: None,
            config_dir: None,
        })
    }

    pub fn with_path(path: Option<PathBuf>) -> io::Result<Self> {
        Ok(Self {
            edit_path: path,
            ..Self::new()?
        })
    }
}

/// Builder-style spec describing a popup to open. Built by the lisp
/// `popup-open` builtin from a widget value plus an options map.
pub struct PopupSpec {
    /// The widget tree drawn inside the popup's placement rect.
    pub widget: Widget,
    pub initial_text: Option<String>,
    pub placement: Placement,
    /// Keymap mode layers to push onto the popup's buffer.
    pub mode_layers: Vec<Rc<str>>,
    pub buffer_mode: EditingMode,
    pub show_cursor: bool,
    pub wrap_mode: WrapMode,
    pub wrap_column: Option<u16>,
    pub breakindent: bool,
}

impl PopupSpec {
    pub fn new(widget: Widget) -> Self {
        Self {
            widget,
            initial_text: None,
            placement: Placement::default(),
            mode_layers: vec![Rc::<str>::from("popup")],
            buffer_mode: EditingMode::Normal,
            show_cursor: false,
            wrap_mode: WrapMode::default(),
            wrap_column: None,
            breakindent: false,
        }
    }
}

pub struct State {
    bufs: BufferList,
    windows: WindowTree,
    focus_minibuffer: bool,
    journal: Journal,
    popups: PopupStack,
    quit: bool,
    keymap: KeymapRegistry,
    keyevents: RingBuffer<(KeyEvent, Instant), 100>,
    keycombo_timeout: Duration,
    count_prefix: CountPrefix,
    renderer: Box<dyn Renderer>,
    /// Embedded lisp runtime. Held as `Option` so `eval_lisp*` can `take` it
    /// for the duration of an eval — this also blocks re-entrant evaluation.
    lisp: Option<LispRuntime>,
    /// Named styles registered by lisp (`face-define`). `RefCell` so render
    /// callbacks can introspect without holding `&mut State`.
    theme: ThemeCell,
    /// Lisp callable that builds the frame's widget tree each render.
    frame_fn: Option<Rc<Value>>,
    workdir: Rc<Path>,
    /// Directory holding `init.rz`. Stored so `reload-config` (and any
    /// future config-path query from lisp) can locate the script after init.
    config_dir: Rc<Path>,
    /// Runtime-registered tree-sitter grammars loaded from shared libraries,
    /// indexed by file extension. Populated by the `grammar-register` lisp
    /// builtin; consulted by [`Self::install_dynamic_highlighter`] when a
    /// buffer with a matching extension is opened.
    ts_registry: TsRegistry,
}

fn resolve_workdir(path: Option<&Path>, cwd: &Path) -> PathBuf {
    match path {
        Some(p) if p.is_dir() => p.to_path_buf(),
        Some(p) => p
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| cwd.to_path_buf()),
        None => cwd.to_path_buf(),
    }
}

/// The directory holding `init.rz` when the caller doesn't override it.
/// In debug/test builds this is the workspace root — where `init.rz` lives
/// in the source tree — so editing the checked-in file is the loop you get.
/// In release builds it's `$XDG_CONFIG_HOME/rizz` (or `~/.config/rizz`).
#[cfg(any(test, debug_assertions))]
fn default_config_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

#[cfg(all(not(test), not(debug_assertions)))]
fn default_config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("rizz")
}

const INIT_SCRIPT_NAME: &str = "init.rz";
const EMBEDDED_INIT_SCRIPT: &str = include_str!("../../../init.rz");

/// Read `<config_dir>/init.rz`, seeding it from the embedded template if it
/// doesn't exist yet so first-run users land on a working config.
fn load_init_script(config_dir: &Path) -> anyhow::Result<String> {
    use std::fs;
    let path = config_dir.join(INIT_SCRIPT_NAME);
    if !path.exists() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, EMBEDDED_INIT_SCRIPT)?;
    }
    Ok(fs::read_to_string(&path)?)
}

#[cfg(any(test, debug_assertions))]
fn init_eval_err(e: RizzError) -> anyhow::Error {
    panic!("init.rz eval failed: {e}");
}

#[cfg(all(not(test), not(debug_assertions)))]
fn init_eval_err(e: RizzError) -> anyhow::Error {
    anyhow::anyhow!(e.to_string())
}

impl State {
    pub fn with_config(config: Config) -> anyhow::Result<Self> {
        let cwd = std::env::current_dir()?;
        let workdir = resolve_workdir(config.edit_path.as_deref(), &cwd);
        let config_dir = config.config_dir.unwrap_or_else(default_config_dir);

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
            frame_fn: None,
            workdir: workdir.into(),
            config_dir: config_dir.into(),
            ts_registry: TsRegistry::new(),
        };
        if let Some(path) = config.edit_path
            && !path.is_dir()
        {
            state.bufs[1] = buffer_io::with_path(Rc::<Path>::from(path));
            state.install_highlighter(1);
        }
        state.refresh_viewport();

        state.run_init_script()?;

        Ok(state)
    }

    /// Locate `init.rz` under the config dir, seed it from the embedded
    /// template if missing, and eval it with the lisp basedir pointed at the
    /// config dir (so `(open "foo.rz")` inside `init.rz` resolves relative to
    /// it). Restores the basedir to the editor workdir on the way out.
    fn run_init_script(&mut self) -> anyhow::Result<()> {
        let src = load_init_script(&self.config_dir)?;
        let config_dir = self.config_dir.clone();
        self.lisp.as_mut().unwrap().set_basedir(config_dir.as_ref());
        let eval_result = self.eval_lisp_script(&src);
        self.lisp
            .as_mut()
            .unwrap()
            .set_basedir(self.workdir.as_ref());
        eval_result.map_err(init_eval_err)
    }

    /// Read `<config_dir>/init.rz` from disk (seeding it from the embedded
    /// template if missing) and return its contents. Used by the lisp
    /// `reload-config` builtin, which evals the result against the live env —
    /// the builtin can't call back into `eval_lisp_script` because the runtime
    /// is already on the stack when a builtin runs.
    pub fn load_init_script(&self) -> anyhow::Result<String> {
        load_init_script(&self.config_dir)
    }

    pub fn config_dir(&self) -> Rc<Path> {
        self.config_dir.clone()
    }

    pub fn init_script_path(&self) -> PathBuf {
        self.config_dir.join(INIT_SCRIPT_NAME)
    }

    /// Install an [`EditorGuard`] and run `f` against the editor's lisp
    /// runtime.
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

    pub fn eval_lisp(&mut self, src: &str) -> Result<Rc<Value>, RizzError> {
        self.with_lisp(|lisp| lisp.eval_str(src))
    }

    pub fn eval_lisp_value(&mut self, form: Rc<Value>) -> Result<Rc<Value>, RizzError> {
        self.with_lisp(|lisp| lisp.eval_value(form))
    }

    pub fn eval_lisp_script(&mut self, src: &str) -> Result<(), RizzError> {
        self.with_lisp(|lisp| lisp.eval_script(src))
    }

    pub fn focused_buf(&self) -> &Buffer {
        let i = self.focused_bufno();
        &self.bufs[i]
    }

    pub fn focused_buf_mut(&mut self) -> &mut Buffer {
        let i = self.focused_bufno();
        &mut self.bufs[i]
    }

    pub fn theme(&self) -> &ThemeCell {
        &self.theme
    }

    pub fn set_frame_fn(&mut self, f: Option<Rc<Value>>) {
        self.frame_fn = f;
    }

    pub fn last_key(&self) -> Option<KeyEvent> {
        self.keyevents.peek_back().map(|(e, _)| e.to_owned())
    }

    pub fn record_message(&mut self, msg: &str) {
        self.journal.record_message(msg);
    }

    pub fn message_history(&self) -> impl Iterator<Item = &Rc<str>> {
        self.journal.messages()
    }

    pub fn record_cmd(&mut self, msg: &str) {
        self.journal.record_command(msg);
    }
    pub fn cmd_history(&self) -> impl Iterator<Item = &Rc<str>> {
        self.journal.commands()
    }

    /// Bridge from Rust-internal failure paths (eval errors, render-callback
    /// errors) to the lisp-side `notify` fn defined in `default.lisp`.
    pub fn notify_via_lisp(&mut self, msg: &str) {
        let src = format!("(notify {})", crate::lisp::quote_for_lisp(msg));
        if let Err(e) = self.eval_lisp(&src) {
            self.record_message(&format!("notify failed: {e}"));
        }
    }

    pub fn top_popup_mode(&self) -> Option<Rc<str>> {
        self.popups.top_mode()
    }

    /// Push a popup onto the overlay stack. Creates a backing buffer of
    /// kind [`BufferKind::Popup`], appends it to `self.bufs`, and returns
    /// its bufno.
    pub fn open_popup(&mut self, spec: PopupSpec) -> usize {
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
            widget: spec.widget,
            mode_layers: spec.mode_layers,
            show_cursor: spec.show_cursor,
        });
        self.refresh_viewport();
        bufno
    }

    pub fn close_popup(&mut self) -> bool {
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

    pub fn top_popup_bufno(&self) -> Option<usize> {
        self.popups.top_bufno()
    }

    pub fn has_popup(&self) -> bool {
        !self.popups.is_empty()
    }

    pub fn set_buffer_contents(&mut self, bufno: usize, msg: &str) {
        let b = &mut self.bufs[bufno];
        b.clear_with(msg);
    }

    /// Read the current minibuffer text and leave the minibuffer.
    pub fn take_minibuffer_command(&mut self) -> String {
        let cmd = self.bufs.minibuffer().text();
        self.exit_minibuffer();
        cmd
    }

    pub fn workdir(&self) -> Rc<Path> {
        self.workdir.clone()
    }

    /// Register a runtime-loaded tree-sitter grammar from a shared library
    /// (`.so` / `.dylib` / `.dll`). Pre-flights the grammar+query by building
    /// a throwaway highlighter — if that errors, the registry isn't touched,
    /// so a bad call doesn't silently break future buffer loads. After
    /// registration, any already-open buffer whose extension matches and has
    /// no highlighter attached gets one installed in place.
    pub fn register_grammar(
        &mut self,
        name: &str,
        extensions: &[String],
        library_path: &Path,
        highlights_query: &str,
    ) -> Result<(), rizz_ts::TsError> {
        self.ts_registry
            .register(name, extensions, library_path, highlights_query)?;
        for i in 0..self.bufs.len() {
            self.install_highlighter(i);
        }
        Ok(())
    }

    /// If `bufno` is a file buffer whose extension matches a registered
    /// dynamic grammar and no highlighter is currently attached, install one.
    /// A buffer that already has a (native) highlighter is left alone.
    fn install_highlighter(&mut self, bufno: usize) {
        if bufno >= self.bufs.len() {
            return;
        }
        if self.bufs[bufno].highlight().is_some() {
            return;
        }
        let Some(path) = self.bufs[bufno].fs_path() else {
            return;
        };
        if let Some(h) = self.ts_registry.highlighter_for_path(&path) {
            self.bufs[bufno].set_highlighter(Some(h));
        }
    }

    pub fn pending_count_or_one(&self) -> u32 {
        self.count_prefix.or_one()
    }

    pub fn keymap_registry(&self) -> &KeymapRegistry {
        &self.keymap
    }

    /// The buffer currently receiving key events.
    pub fn focused_bufno(&self) -> usize {
        if let Some(bufno) = self.popups.top_bufno() {
            return bufno;
        }
        if self.focus_minibuffer {
            self.bufs.minibuffer_index()
        } else {
            self.windows.focused_bufno()
        }
    }

    pub fn nbufs(&self) -> usize {
        self.bufs.len()
    }

    pub fn minibuffer_bufno(&self) -> usize {
        self.bufs.minibuffer_index()
    }

    /// Update viewports of all buffers currently displayed in a window,
    /// the minibuffer, and every popup.
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
        let popups: Vec<(usize, Position<u16>)> = self
            .popups
            .iter()
            .map(|p| {
                let buf = &self.bufs[p.bufno];
                let outer = rizz_ui::popup::resolve_popup_rect(p, editor_area, buf);
                let inner = rizz_ui::popup::buffer_view_rect(&p.widget, outer, p.bufno);
                (p.bufno, Position::new(inner.width, inner.height))
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

        let ke: KeyEvent = event.into();
        if self.count_prefix.feed(ke, self.count_eligible()) {
            self.refresh_viewport();
            return self.render();
        }

        let modes = self.bufs[self.focused_bufno()].active_modes();
        if let Some(action) = self.keymap.resolve(&modes, ke, timedout) {
            self.apply(&action)?;
            self.count_prefix.clear();
        }
        self.refresh_viewport();
        let focused = self.focused_bufno();
        self.bufs[focused].clamp_cursor();
        self.render()
    }

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
                Action::InsertMany(s) => {
                    let f = self.focused_bufno();
                    self.bufs[f].insert_many(s);
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
                Action::DeleteSelection => {
                    let f = self.focused_bufno();
                    self.bufs[f].delete_selection();
                }
                Action::DeleteLine { count } => {
                    let f = self.focused_bufno();
                    self.bufs[f].delete_line(*count);
                }
                Action::DeleteMotion { kind, count } => {
                    let f = self.focused_bufno();
                    self.bufs[f].delete_motion(*kind, *count);
                }
                Action::Undo => {
                    let f = self.focused_bufno();
                    self.bufs[f].undo();
                }
                Action::Redo => {
                    let f = self.focused_bufno();
                    self.bufs[f].redo();
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

    fn set_mode(&mut self, mode: EditingMode) {
        if mode == EditingMode::Command {
            self.bufs.minibuffer_mut().clear();
            self.bufs.minibuffer_mut().set_mode(EditingMode::Command);
            self.focus_minibuffer = true;
        } else {
            let f = self.focused_bufno();
            self.bufs[f].set_mode(mode);
        }
    }

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
        self.install_highlighter(bufno);
        if set_active {
            self.windows.set_focused_bufno(bufno);
        }
        Ok(bufno)
    }

    fn edit_buf(&mut self, path: Rc<Path>) -> io::Result<usize> {
        let idx = match self.bufs.find_by_path(&path) {
            Some(idx) => idx,
            None => {
                let pushed = self.bufs.push(buffer_io::with_path(path));
                self.install_highlighter(pushed);
                pushed
            }
        };
        self.windows.set_focused_bufno(idx);
        Ok(idx)
    }

    fn write_buf(&mut self, path: Option<Rc<Path>>) -> io::Result<()> {
        let editor = self.windows.focused_bufno();
        buffer_io::write(&mut self.bufs[editor], path)
    }

    fn delete_buf(&mut self, bufno: usize) {
        if bufno >= self.bufs.len() || self.bufs[bufno].kind() == BufferKind::Minibuffer {
            return;
        }

        if self.bufs.file_buf_count() == 1 {
            self.bufs.reset(bufno);
            self.windows.for_each_leaf_mut(|b| *b = bufno);
            return;
        }

        self.bufs.remove(bufno);
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
        self.bufs.push(Buffer::new());
        let new_bufno = self.bufs.len() - 1;
        self.windows.split(dir, new_bufno);
    }

    fn window_close(&mut self) {
        self.windows.close_focused();
    }

    fn cycle_buffer(&mut self, dir: CycleDir) {
        if let Some(i) = self.bufs.cycle(self.windows.focused_bufno(), dir) {
            self.windows.set_focused_bufno(i);
        }
    }

    pub fn render(&mut self) -> io::Result<()> {
        let focused = self.focused_bufno();
        let (frame, error_msg) = self.precompute_frame();
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
        if let Some(msg) = error_msg {
            self.notify_via_lisp(&msg);
        }
        result
    }

    /// Run every region under an `EditorGuard`, packing the results into a
    /// `RenderedFrame` the renderer can consume without ever touching lisp.
    pub fn precompute_frame(&mut self) -> (RenderedFrame, Option<String>) {
        // Bring every buffer's syntax tree up to date before precompute walks
        // them immutably. `refresh_highlight` short-circuits when no language
        // is attached or the tree is already clean.
        for b in self.bufs.as_mut_slice() {
            b.refresh_highlight();
        }

        let lisp = self.lisp.take().expect("recursive render is not supported");
        let _editor_guard = EditorGuard::new(self);
        let _phase_guard = RenderPhaseGuard::enter();

        let result = precompute::compute(precompute::PrecomputeInput {
            bufs: self.bufs.as_slice(),
            windows: &self.windows,
            frame_fn: self.frame_fn.as_ref(),
            theme: &self.theme,
            minibuffer: self.bufs.minibuffer_index(),
            lisp_env: lisp.env(),
        });

        drop(_phase_guard);
        drop(_editor_guard);
        self.lisp = Some(lisp);

        result
    }
}

// Test support — `pub` so the lisp module's tests can use it.
pub mod test_support {
    use super::*;
    use rizz_ui::render::{Renderer, StateSnapshot};

    /// A renderer that does nothing — used in tests that don't want to touch a terminal.
    pub struct NullRenderer;
    impl Renderer for NullRenderer {
        fn render(&mut self, _snap: StateSnapshot<'_>, _frame: &RenderedFrame) -> io::Result<()> {
            Ok(())
        }
    }

    pub fn test_state() -> State {
        State::with_config(Config {
            renderer: Box::new(NullRenderer),
            keycombo_timeout: Duration::from_hours(24),
            edit_path: None,
            config_dir: None,
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

    fn top_popup_text(s: &State) -> String {
        let bufno = s.top_popup_bufno().expect("popup is visible");
        s.bufs[bufno].text()
    }

    #[test]
    fn notify_records_history_and_shows_popup() {
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
        use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
        let mut s = test_state();
        s.eval_lisp(r#"(notify "oops")"#).unwrap();
        assert!(s.has_popup());
        s.handle_key_event(CT::new(KeyCode::Char('q'), KeyModifiers::NONE))
            .unwrap();
        assert!(!s.has_popup());
    }

    #[test]
    fn count_prefix_scales_motion() {
        use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
        let mut s = test_state();
        s.bufs[1].clear_with("a\nb\nc\nd\ne\nf\ng");
        s.handle_key_event(CT::new(KeyCode::Char('3'), KeyModifiers::NONE))
            .unwrap();
        let abs_row = s.bufs[1].cursor_pos().row as usize + s.bufs[1].file_pos().row;
        assert_eq!(abs_row, 0);
        s.handle_key_event(CT::new(KeyCode::Char('j'), KeyModifiers::NONE))
            .unwrap();
        let abs_row = s.bufs[1].cursor_pos().row as usize + s.bufs[1].file_pos().row;
        assert_eq!(abs_row, 3);
    }

    #[test]
    fn leading_zero_falls_through_as_line_start() {
        use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
        let mut s = test_state();
        s.bufs[1].clear_with("hello world");
        s.handle_key_event(CT::new(KeyCode::Char('l'), KeyModifiers::NONE))
            .unwrap();
        s.handle_key_event(CT::new(KeyCode::Char('l'), KeyModifiers::NONE))
            .unwrap();
        s.handle_key_event(CT::new(KeyCode::Char('l'), KeyModifiers::NONE))
            .unwrap();
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
    fn default_precompute_produces_expected_frame() {
        let mut s = test_state();
        let (frame, err) = s.precompute_frame();
        assert!(err.is_none(), "no frame errors expected: {err:?}");
        let bufno = s.windows.focused_bufno();
        let bf = &frame.per_buf[bufno];
        assert!(bf.gutter.is_some(), "expected a gutter");
        assert!(bf.decorators.len() >= 3, "expected the 3 built-in passes");
    }

    #[test]
    fn register_grammar_rejects_missing_library() {
        let mut s = test_state();
        // A non-existent library path should fail the pre-flight in
        // `register_grammar` and leave the registry untouched.
        let err = s.register_grammar(
            "fake",
            &["fake".to_string()],
            Path::new("/path/does/not/exist.dylib"),
            "; empty query",
        );
        assert!(err.is_err(), "expected library load failure, got Ok");
        assert!(s.ts_registry.is_empty(), "registry must stay empty");
    }
}
