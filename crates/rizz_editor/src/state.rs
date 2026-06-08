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
use tracing::{debug, error, info, instrument, trace, warn};

use rizz_actions::{Action, KeymapRegistry};
use rizz_core::{EditingMode, Position};
use rizz_input::{CountPrefix, KeyEvent};
use rizz_text::{Buffer, BufferId, WrapMode, io as buffer_io};
use rizz_ts::TsRegistry;

use rizz_ui::{
    RatatuiRenderer, Renderer, StateSnapshot, ThemeCell, Widget, WindowTree,
    panel::{Panel, PanelKind, PanelStack, Placement},
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
    journal: Journal,
    /// Stack of input/overlay panels above the window tree. Includes the
    /// minibuffer (pushed on `set-mode 'command`) and any open popups.
    /// When empty, focus falls through to the focused window leaf.
    panels: PanelStack,
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
        info!(
            workdir = %workdir.display(),
            config_dir = %config_dir.display(),
            edit_path = ?config.edit_path,
            "constructing State"
        );

        let bufs = BufferList::new();
        let first_file = bufs.first_file_buf();
        let mut state = Self {
            bufs,
            windows: WindowTree::new(first_file),
            journal: Journal::new(),
            panels: PanelStack::new(),
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
            state.bufs[first_file] = buffer_io::with_path(Rc::<Path>::from(path));
            state.install_highlighter(first_file);
        }
        state.refresh_viewport();

        state.run_init_script()?;

        Ok(state)
    }

    /// Locate `init.rz` under the config dir, seed it from the embedded
    /// template if missing, and eval it with the lisp basedir pointed at the
    /// config dir (so `(open "foo.rz")` inside `init.rz` resolves relative to
    /// it). Restores the basedir to the editor workdir on the way out.
    #[instrument(skip(self), fields(config_dir = %self.config_dir.display()))]
    fn run_init_script(&mut self) -> anyhow::Result<()> {
        let src = load_init_script(&self.config_dir)?;
        debug!(bytes = src.len(), "loaded init.rz");
        let config_dir = self.config_dir.clone();
        self.lisp.as_mut().unwrap().set_basedir(config_dir.as_ref());
        let eval_result = self.eval_lisp_script(&src);
        self.lisp
            .as_mut()
            .unwrap()
            .set_basedir(self.workdir.as_ref());
        if let Err(e) = &eval_result {
            error!(error = %e, "init.rz eval failed");
        } else {
            info!("init.rz eval ok");
        }
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

    #[instrument(skip(self, src), fields(bytes = src.len()))]
    pub fn eval_lisp(&mut self, src: &str) -> Result<Rc<Value>, RizzError> {
        trace!(src = %src.chars().take(200).collect::<String>(), "eval_lisp src");
        let r = self.with_lisp(|lisp| lisp.eval_str(src));
        match &r {
            Ok(v) => trace!(result = %v.display(), "eval_lisp ok"),
            Err(e) => warn!(error = %e, "eval_lisp err"),
        }
        r
    }

    #[instrument(skip(self, form))]
    pub fn eval_lisp_value(&mut self, form: Rc<Value>) -> Result<Rc<Value>, RizzError> {
        trace!(form = %form.display(), "eval_lisp_value form");
        let r = self.with_lisp(|lisp| lisp.eval_value(form));
        if let Err(e) = &r {
            warn!(error = %e, "eval_lisp_value err");
        }
        r
    }

    #[instrument(skip(self, src), fields(bytes = src.len()))]
    pub fn eval_lisp_script(&mut self, src: &str) -> Result<(), RizzError> {
        let r = self.with_lisp(|lisp| lisp.eval_script(src));
        if let Err(e) = &r {
            warn!(error = %e, "eval_lisp_script err");
        }
        r
    }

    pub fn focused_buf(&self) -> &Buffer {
        let id = self.focused_buf_id();
        &self.bufs[id]
    }

    pub fn focused_buf_mut(&mut self) -> &mut Buffer {
        let id = self.focused_buf_id();
        &mut self.bufs[id]
    }

    pub fn theme(&self) -> &ThemeCell {
        &self.theme
    }

    pub fn set_frame_fn(&mut self, f: Option<Rc<Value>>) {
        self.frame_fn = f;
    }

    pub fn get_frame_fn(&self) -> Option<&Rc<Value>> {
        self.frame_fn.as_ref()
    }

    pub fn last_key(&self) -> Option<KeyEvent> {
        self.keyevents.peek_back().map(|(e, _)| e.to_owned())
    }

    pub fn record_message(&mut self, msg: &str) {
        info!(target: "rizz::journal", msg, "journal: message");
        self.journal.record_message(msg);
    }

    pub fn message_history(&self) -> impl Iterator<Item = &Rc<str>> {
        self.journal.messages()
    }

    pub fn record_cmd(&mut self, msg: &str) {
        info!(target: "rizz::journal", cmd = msg, "journal: command");
        self.journal.record_command(msg);
    }
    pub fn cmd_history(&self) -> impl Iterator<Item = &Rc<str>> {
        self.journal.commands()
    }

    /// Bridge from Rust-internal failure paths (eval errors, render-callback
    /// errors) to the lisp-side `notify` fn defined in `default.lisp`.
    pub fn notify_via_lisp(&mut self, msg: &str) {
        debug!(msg, "notify_via_lisp");
        let src = format!("(notify {})", crate::lisp::quote_for_lisp(msg));
        if let Err(e) = self.eval_lisp(&src) {
            error!(error = %e, "notify-via-lisp failed");
            self.record_message(&format!("notify failed: {e}"));
        }
    }

    /// Topmost keymap layer of the topmost *overlay* panel, if any. Used by
    /// the `popup-mode` lisp builtin to detect "am I inside a popup of kind X?"
    pub fn top_popup_mode(&self) -> Option<Rc<str>> {
        self.panels
            .top_overlay()
            .and_then(|p| p.keymap_layers.last().cloned())
    }

    /// Push an overlay panel onto the stack. Creates a fresh panel-backing
    /// buffer (not in the file cycle), appends it to `self.bufs`, and
    /// returns its `BufferId`.
    #[instrument(skip(self, spec), fields(
        modes = ?spec.mode_layers,
        buffer_mode = ?spec.buffer_mode,
        wrap_mode = ?spec.wrap_mode,
    ))]
    pub fn open_popup(&mut self, spec: PopupSpec) -> BufferId {
        let mut buf = Buffer::new();
        if let Some(text) = spec.initial_text {
            buf.clear_with(&text);
        }
        buf.set_mode(spec.buffer_mode);
        buf.set_wrap_mode(spec.wrap_mode);
        buf.set_wrap_column(spec.wrap_column);
        buf.set_breakindent(spec.breakindent);
        let id = self.bufs.push_panel(buf);
        self.panels.push(Panel {
            buf: id,
            keymap_layers: spec.mode_layers,
            kind: PanelKind::Overlay {
                placement: spec.placement,
                widget: spec.widget,
                show_cursor: spec.show_cursor,
            },
        });
        self.refresh_viewport();
        info!(?id, "overlay panel opened");
        id
    }

    /// Close the topmost overlay panel (skipping a minibuffer panel if it
    /// sits on top). Removes the overlay's backing buffer and redirects any
    /// window leaves that pointed at it to the first file buffer.
    #[instrument(skip(self))]
    pub fn close_popup(&mut self) -> bool {
        let Some(panel) = self.panels.pop_top_overlay() else {
            trace!("no overlay to close");
            return false;
        };
        let removed = panel.buf;
        info!(?removed, "closing overlay");
        // Only clean up the backing buffer if it's not a file buffer (those
        // outlive their panel — closing a popup that happened to be viewing
        // file buf 2 shouldn't delete file buf 2).
        if !self.bufs.is_file_buf(removed) && removed != self.bufs.minibuffer_id() {
            self.bufs.remove(removed);
            let first = self.bufs.first_file_buf();
            self.windows.for_each_leaf_mut(|b| {
                if *b == removed {
                    *b = first;
                }
            });
        }
        self.refresh_viewport();
        true
    }

    pub fn top_popup_buf(&self) -> Option<BufferId> {
        self.panels.top_overlay().map(|p| p.buf)
    }

    pub fn has_popup(&self) -> bool {
        self.panels.any_overlay()
    }

    pub fn set_buffer_contents(&mut self, buf: BufferId, msg: &str) {
        if let Some(b) = self.bufs.get_mut(buf) {
            b.clear_with(msg);
        }
    }

    /// Read the current minibuffer text and leave the minibuffer.
    pub fn take_minibuffer_command(&mut self) -> String {
        let cmd = self.bufs.minibuffer().text();
        self.exit_minibuffer();
        cmd
    }

    /// The substring of the minibuffer token that ends at the cursor — what
    /// candidate completions must `starts_with`. Always operates on the
    /// minibuffer regardless of which buffer currently has focus, since a
    /// completion popup may have stolen focus while the cmd line is still up.
    pub fn minibuffer_completion_prefix(&self) -> String {
        let mb = self.bufs.minibuffer();
        crate::completion::prefix_at(&mb.text(), mb.abs_col())
    }

    /// Replace the token under the minibuffer cursor with `replacement`,
    /// landing the cursor at the end of the inserted text. Falls back to a
    /// plain insert when the cursor isn't on a token. Operates on the
    /// minibuffer directly — see [`Self::minibuffer_completion_prefix`].
    pub fn apply_minibuffer_completion(&mut self, replacement: &str) {
        let (text, cursor) = {
            let mb = self.bufs.minibuffer();
            (mb.text(), mb.abs_col())
        };
        let (start, end) = crate::completion::token_bounds(&text, cursor);
        let mb = self.bufs.minibuffer_mut();
        if start < end {
            mb.delete_range(start, end);
        }
        mb.insert_many(replacement);
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
    #[instrument(skip(self, highlights_query), fields(
        library_path = %library_path.display(),
        query_bytes = highlights_query.len(),
    ))]
    pub fn register_grammar(
        &mut self,
        name: &str,
        extensions: &[String],
        library_path: &Path,
        highlights_query: &str,
    ) -> Result<(), rizz_ts::TsError> {
        if let Err(e) = self
            .ts_registry
            .register(name, extensions, library_path, highlights_query)
        {
            error!(error = %e, "register_grammar failed");
            return Err(e);
        }
        info!(?extensions, "registered grammar");
        let ids: Vec<BufferId> = self.bufs.iter().map(|(id, _)| id).collect();
        for id in ids {
            self.install_highlighter(id);
        }
        Ok(())
    }

    /// If `buf` is a file buffer whose extension matches a registered
    /// dynamic grammar and no highlighter is currently attached, install one.
    /// A buffer that already has a (native) highlighter is left alone.
    fn install_highlighter(&mut self, buf: BufferId) {
        if !self.bufs.contains(buf) {
            return;
        }
        if self.bufs[buf].highlight().is_some() {
            return;
        }
        let Some(path) = self.bufs[buf].fs_path() else {
            return;
        };
        if let Some(h) = self.ts_registry.highlighter_for_path(&path) {
            self.bufs[buf].set_highlighter(Some(h));
        }
    }

    pub fn pending_count_or_one(&self) -> u32 {
        self.count_prefix.or_one()
    }

    pub fn keymap_registry(&self) -> &KeymapRegistry {
        &self.keymap
    }

    /// The buffer currently receiving key events. Reads from the panel
    /// stack (popups + minibuffer-when-focused) before falling through to
    /// the focused editor window leaf.
    pub fn focused_buf_id(&self) -> BufferId {
        match self.panels.top_buf() {
            Some(id) => id,
            None => self.windows.focused_buf(),
        }
    }

    /// Active keymap modes for the focused input context, most-specific first.
    /// The top panel's named layers (e.g. `"popup"`, `"popup.files"`) precede
    /// the focused buffer's [`EditingMode`]. When no panel is on the stack,
    /// this is just `[buf.mode]`.
    fn active_modes(&self) -> Vec<Rc<str>> {
        let mut v: Vec<Rc<str>> = self
            .panels
            .top_keymap_layers()
            .iter()
            .rev()
            .cloned()
            .collect();
        let mode = self.bufs[self.focused_buf_id()].mode();
        v.push(mode.as_str().into());
        v
    }

    pub fn nbufs(&self) -> usize {
        self.bufs.len()
    }

    pub fn minibuffer_id(&self) -> BufferId {
        self.bufs.minibuffer_id()
    }

    pub fn buf_exists(&self, id: BufferId) -> bool {
        self.bufs.contains(id)
    }

    /// 1-based display index of `id` among file buffers, or `None` for the
    /// minibuffer / popup-backing buffers / unknown ids.
    pub fn buf_display_index(&self, id: BufferId) -> Option<usize> {
        self.bufs.file_display_index(id)
    }

    /// Update viewports of all buffers currently displayed in a window,
    /// the minibuffer, and every overlay panel.
    fn refresh_viewport(&mut self) {
        let Ok((cols, rows)) = crossterm::terminal::size() else {
            return;
        };
        let editor_h = rows.saturating_sub(STATUS_LINE_ROWS + MINIBUFFER_ROWS);
        let editor_area = ratatui::layout::Rect::new(0, 0, cols, editor_h);
        for leaf in self.windows.layout(editor_area) {
            if let Some(buf) = self.bufs.get_mut(leaf.buf) {
                buf.viewport = Position::new(leaf.area.width, leaf.area.height);
            }
        }
        self.bufs.minibuffer_mut().viewport = Position::new(cols, MINIBUFFER_ROWS);
        let overlay_viewports: Vec<(BufferId, Position<u16>)> = self
            .panels
            .overlays()
            .filter_map(|p| {
                let (_, widget, _) = p.as_overlay()?;
                let buf = &self.bufs[p.buf];
                let outer = rizz_ui::panel::resolve_overlay_rect(p, editor_area, buf);
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

    pub fn quit_requested(&self) -> bool {
        self.quit
    }

    #[instrument(skip(self), fields(code = ?event.code, mods = ?event.modifiers))]
    pub fn handle_key_event(&mut self, event: CTKeyEvent) -> io::Result<()> {
        let now = Instant::now();
        let timedout = self
            .keyevents
            .peek_back()
            .is_some_and(|(_, earlier)| now.duration_since(*earlier) > self.keycombo_timeout);
        self.keyevents.push_back((event.into(), now));

        let ke: KeyEvent = event.into();
        if self.count_prefix.feed(ke, self.count_eligible()) {
            trace!(?ke, "key consumed by count prefix");
            self.refresh_viewport();
            return self.render();
        }

        let modes = self.active_modes();
        debug!(?ke, ?modes, timedout, "resolving key against keymap");
        if let Some(action) = self.keymap.resolve(&modes, ke, timedout) {
            debug!(
                actions = action.len(),
                "keymap resolved -> applying actions"
            );
            self.apply(&action)?;
            self.count_prefix.clear();
        } else {
            trace!(?ke, "no action resolved (descent or miss)");
        }
        self.refresh_viewport();
        let focused = self.focused_buf_id();
        self.bufs[focused].clamp_cursor();
        self.render()
    }

    fn count_eligible(&self) -> bool {
        // Count prefix is only honoured for editor windows in a non-visual
        // editing mode — any panel on the stack means input is going somewhere
        // else (popup, minibuffer) and digits should pass through verbatim.
        if !self.panels.is_empty() {
            return false;
        }
        if !self.keymap.is_idle() {
            return false;
        }
        matches!(
            self.bufs[self.focused_buf_id()].mode(),
            EditingMode::Normal
                | EditingMode::Visual
                | EditingMode::VisualLine
                | EditingMode::VisualBlock
        )
    }

    #[instrument(skip(self, actions), fields(count = actions.len()))]
    pub fn apply(&mut self, actions: &[Rc<Action>]) -> io::Result<()> {
        for action in actions {
            trace!(action = ?action.as_ref(), "applying action");
            match action.as_ref() {
                Action::Noop => {}
                Action::Quit => {
                    info!("Action::Quit -> set quit flag");
                    self.quit = true;
                }
                Action::SetMode(m) => {
                    debug!(mode = ?m, "Action::SetMode");
                    self.set_mode(*m);
                }
                Action::InsertChar(c) => {
                    let f = self.focused_buf_id();
                    self.bufs[f].insert_char(*c);
                }
                Action::SpeculativeInsertChar(c) => {
                    let f = self.focused_buf_id();
                    self.bufs[f].insert_speculative_char(*c);
                }
                Action::CommitSpeculation => {
                    let f = self.focused_buf_id();
                    self.bufs[f].commit_speculation();
                }
                Action::RollbackSpeculation => {
                    let f = self.focused_buf_id();
                    self.bufs[f].rollback_speculation();
                }
                Action::InsertMany(s) => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, len = s.len(), "Action::InsertMany");
                    self.bufs[f].insert_many(s);
                }
                Action::InsertNewline => {
                    let f = self.focused_buf_id();
                    self.bufs[f].insert_char('\n');
                }
                Action::DeleteChar => {
                    let f = self.focused_buf_id();
                    self.bufs[f].delete_char();
                }
                Action::DeleteCharAt(pos) => {
                    let f = self.focused_buf_id();
                    self.bufs[f].delete_char_at(*pos);
                }
                Action::DeleteSelection => {
                    let f = self.focused_buf_id();
                    self.bufs[f].delete_selection();
                }
                Action::DeleteLine { count } => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, count, "Action::DeleteLine");
                    self.bufs[f].delete_line(*count);
                }
                Action::DeleteMotion { kind, count } => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, ?kind, count, "Action::DeleteMotion");
                    self.bufs[f].delete_motion(*kind, *count);
                }
                Action::Undo => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, "Action::Undo");
                    self.bufs[f].undo();
                }
                Action::Redo => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, "Action::Redo");
                    self.bufs[f].redo();
                }
                Action::MoveCursor { kind, count } => {
                    let f = self.focused_buf_id();
                    trace!(buf = ?f, ?kind, count, "Action::MoveCursor");
                    self.bufs[f].move_cursor_n(*kind, *count);
                }
                Action::CommandCancel => {
                    debug!("Action::CommandCancel");
                    self.exit_minibuffer();
                }
                Action::BufCreate { path, set_active } => {
                    info!(?path, set_active, "Action::BufCreate");
                    self.create_buf(*set_active, path.clone())?;
                }
                Action::BufDelete => {
                    let editor = self.windows.focused_buf();
                    info!(buf = ?editor, "Action::BufDelete");
                    self.delete_buf(editor);
                }
                Action::BufNext => {
                    debug!("Action::BufNext");
                    self.cycle_buffer(CycleDir::Next);
                }
                Action::BufPrev => {
                    debug!("Action::BufPrev");
                    self.cycle_buffer(CycleDir::Prev);
                }
                Action::BufEdit(path) => {
                    info!(?path, "Action::BufEdit");
                    self.edit_buf(path.clone())?;
                }
                Action::BufWrite(path) => {
                    info!(?path, "Action::BufWrite");
                    self.write_buf(path.clone())?;
                }
                Action::WindowSplit(dir) => {
                    info!(?dir, "Action::WindowSplit");
                    self.window_split(*dir);
                }
                Action::WindowClose => {
                    info!("Action::WindowClose");
                    self.window_close();
                }
                Action::WindowFocusNext => {
                    debug!("Action::WindowFocusNext");
                    self.windows.focus_next();
                }
                Action::WindowFocus(d) => {
                    debug!(dir = ?d, "Action::WindowFocus");
                    self.windows.focus_dir(*d);
                }
                Action::KeymapSet { mode, lhs, rhs } => {
                    debug!(%mode, keys = lhs.len(), "Action::KeymapSet");
                    self.keymap.set(mode.clone(), lhs, rhs.clone());
                }
                Action::KeymapRemove { mode, lhs } => {
                    debug!(%mode, keys = lhs.len(), "Action::KeymapRemove");
                    self.keymap.remove(mode.clone(), lhs);
                }
                Action::EvalLisp(form) => {
                    if let Err(e) = self.eval_lisp_value(form.clone()) {
                        warn!(error = %e, "Action::EvalLisp failed -> notifying");
                        self.notify_via_lisp(&e.to_string());
                    }
                }
            }
        }
        Ok(())
    }

    fn set_mode(&mut self, mode: EditingMode) {
        if mode == EditingMode::Command {
            debug!("entering command mode (focusing minibuffer)");
            self.bufs.minibuffer_mut().clear();
            self.bufs.minibuffer_mut().set_mode(EditingMode::Command);
            // Only push a fresh minibuffer panel if one isn't already on the
            // stack — re-entering command mode while already there is a no-op.
            if !self.panels.iter().any(|p| p.is_minibuffer()) {
                let mb = self.bufs.minibuffer_id();
                self.panels.push(Panel::minibuffer(mb));
            }
        } else {
            let f = self.focused_buf_id();
            debug!(buf = ?f, ?mode, "setting buffer mode");
            self.bufs[f].set_mode(mode);
        }
    }

    fn exit_minibuffer(&mut self) {
        debug!("exiting minibuffer");
        self.bufs.minibuffer_mut().clear();
        self.bufs.minibuffer_mut().set_mode(EditingMode::Command);
        self.panels.pop_minibuffer();
        let editor = self.windows.focused_buf();
        self.bufs[editor].set_mode(EditingMode::Normal);
    }

    #[instrument(skip(self))]
    fn create_buf(&mut self, set_active: bool, path: Option<Rc<Path>>) -> io::Result<BufferId> {
        let buf = match path {
            Some(p) => self.bufs.buffer_for_path(p),
            None => Buffer::new(),
        };
        let id = self.bufs.push_file(buf);
        self.install_highlighter(id);
        if set_active {
            self.windows.set_focused_buf(id);
        }
        info!(buf = ?id, set_active, "created buffer");
        Ok(id)
    }

    #[instrument(skip(self))]
    fn edit_buf(&mut self, path: Rc<Path>) -> io::Result<BufferId> {
        let id = match self.bufs.find_by_path(&path) {
            Some(id) => {
                debug!(buf = ?id, "edit_buf: reusing existing buffer");
                id
            }
            None => {
                let pushed = self.bufs.push_file(buffer_io::with_path(path));
                self.install_highlighter(pushed);
                info!(buf = ?pushed, "edit_buf: created new buffer");
                pushed
            }
        };
        self.windows.set_focused_buf(id);
        Ok(id)
    }

    #[instrument(skip(self))]
    fn write_buf(&mut self, path: Option<Rc<Path>>) -> io::Result<()> {
        let editor = self.windows.focused_buf();
        let r = buffer_io::write(&mut self.bufs[editor], path);
        if let Err(e) = &r {
            error!(buf = ?editor, error = %e, "write_buf failed");
        } else {
            info!(buf = ?editor, "wrote buffer");
        }
        r
    }

    #[instrument(skip(self))]
    fn delete_buf(&mut self, buf: BufferId) {
        if !self.bufs.contains(buf) {
            warn!(?buf, "delete_buf: skipping (unknown id)");
            return;
        }
        if !self.bufs.is_file_buf(buf) {
            warn!(?buf, "delete_buf: skipping (not a file buffer)");
            return;
        }

        if self.bufs.file_buf_count() == 1 {
            debug!(?buf, "delete_buf: last file buffer -> resetting");
            self.bufs.reset(buf);
            self.windows.for_each_leaf_mut(|b| *b = buf);
            return;
        }

        self.bufs.remove(buf);
        let first = self.bufs.first_file_buf();
        self.windows.for_each_leaf_mut(|b| {
            if *b == buf {
                *b = first;
            }
        });
        info!(?buf, "deleted buffer");
    }

    fn window_split(&mut self, dir: SplitDir) {
        let new_buf = self.bufs.push_file(Buffer::new());
        self.windows.split(dir, new_buf);
        info!(?dir, ?new_buf, "window split");
    }

    fn window_close(&mut self) {
        debug!("closing focused window");
        self.windows.close_focused();
    }

    fn cycle_buffer(&mut self, dir: CycleDir) {
        if let Some(id) = self.bufs.cycle(self.windows.focused_buf(), dir) {
            debug!(?dir, buf = ?id, "cycled buffer");
            self.windows.set_focused_buf(id);
        } else {
            trace!(?dir, "cycle_buffer: no cycle (single file buffer)");
        }
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
            windows: &self.windows,
            minibuffer: self.bufs.minibuffer(),
            buf: self.windows.focused_buf(),
            keyevent: self.keyevents.peek_back().map(|(e, _)| e.to_owned()),
            cursor_style: match self.bufs[focused].mode() {
                EditingMode::Insert | EditingMode::Command => CursorStyle::Bar,
                _ => CursorStyle::Block,
            },
            panels: &self.panels,
        };
        let result = self.renderer.render(snap, &frame);
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
        // Bring every buffer's syntax tree up to date before precompute walks
        // them immutably. `refresh_highlight` short-circuits when no language
        // is attached or the tree is already clean.
        for (_, b) in self.bufs.iter_mut() {
            b.refresh_highlight();
        }

        let lisp = self.lisp.take().expect("recursive render is not supported");
        let _editor_guard = EditorGuard::new(self);
        let _phase_guard = RenderPhaseGuard::enter();

        let result = precompute::compute(precompute::PrecomputeInput {
            bufs: self.bufs.raw(),
            windows: &self.windows,
            frame_fn: self.frame_fn.as_ref(),
            theme: &self.theme,
            minibuffer: self.bufs.minibuffer_id(),
            file_bufs: self.bufs.file_ids(),
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
        let id = s.top_popup_buf().expect("popup is visible");
        s.bufs[id].text()
    }

    /// The first file buffer's id. Tests use it to address "the" editor
    /// buffer the way they used to address `s.bufs[1]`.
    fn primary(s: &State) -> BufferId {
        s.bufs.first_file_buf()
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
        let b = primary(&s);
        s.bufs[b].clear_with("a\nb\nc\nd\ne\nf\ng");
        s.handle_key_event(CT::new(KeyCode::Char('3'), KeyModifiers::NONE))
            .unwrap();
        let abs_row = s.bufs[b].cursor_pos().row as usize + s.bufs[b].file_pos().row;
        assert_eq!(abs_row, 0);
        s.handle_key_event(CT::new(KeyCode::Char('j'), KeyModifiers::NONE))
            .unwrap();
        let abs_row = s.bufs[b].cursor_pos().row as usize + s.bufs[b].file_pos().row;
        assert_eq!(abs_row, 3);
    }

    #[test]
    fn leading_zero_falls_through_as_line_start() {
        use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
        let mut s = test_state();
        let b = primary(&s);
        s.bufs[b].clear_with("hello world");
        s.handle_key_event(CT::new(KeyCode::Char('l'), KeyModifiers::NONE))
            .unwrap();
        s.handle_key_event(CT::new(KeyCode::Char('l'), KeyModifiers::NONE))
            .unwrap();
        s.handle_key_event(CT::new(KeyCode::Char('l'), KeyModifiers::NONE))
            .unwrap();
        s.handle_key_event(CT::new(KeyCode::Char('0'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(s.bufs[b].cursor_pos().col, 0);
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
        let id = s.windows.focused_buf();
        let bf = &frame.per_buf[id];
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

    #[test]
    fn can_insert_j() {
        use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
        let mut s = test_state();
        let b = primary(&s);
        s.bufs[b].clear();
        s.set_mode(EditingMode::Insert);
        s.handle_key_event(CT::new(KeyCode::Char('j'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(s.bufs[b].text(), "j".to_string())
    }

    #[test]
    fn jk_chord_rolls_back_speculative_j() {
        // Typing the full `jk` escape chord must leave the buffer empty
        // (speculation rolled back) and switch to normal mode.
        use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
        let mut s = test_state();
        let b = primary(&s);
        s.bufs[b].clear();
        s.set_mode(EditingMode::Insert);
        s.handle_key_event(CT::new(KeyCode::Char('j'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(s.bufs[b].text(), "j".to_string());
        s.handle_key_event(CT::new(KeyCode::Char('k'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(s.bufs[b].text(), "".to_string());
        assert_eq!(s.bufs[b].mode(), EditingMode::Normal);
    }

    #[test]
    fn aborted_jk_chord_commits_speculative_j() {
        // Typing `j` then a non-`k` key commits the speculation and inserts
        // the new key. The two should end up as one undo step.
        use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
        let mut s = test_state();
        let b = primary(&s);
        s.bufs[b].clear();
        s.set_mode(EditingMode::Insert);
        s.handle_key_event(CT::new(KeyCode::Char('j'), KeyModifiers::NONE))
            .unwrap();
        s.handle_key_event(CT::new(KeyCode::Char('x'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(s.bufs[b].text(), "jx".to_string());
        // Both chars belong to the same insert run.
        s.bufs[b].undo();
        assert_eq!(s.bufs[b].text(), "".to_string());
    }
}
