//! The editor process — single owner of every long-lived editor field.
//!
//! `State::apply` is the only mutator: every key event, lisp call, or
//! external trigger ultimately produces an [`rizz_actions::Action`] list and
//! sends it through here. The single-funnel invariant is what makes undo,
//! scripting, and tests tractable.

use std::collections::HashSet;
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
use rizz_registers::{RegisterEntry, Registers};
use rizz_search::{Search, SearchDir, SearchHost, SearchOrigin};
use rizz_text::{Buffer, BufferId, WrapMode, io as buffer_io};
use rizz_ts::TsRegistry;
use rizz_ts_install::{InstallOpts, Manifest as GrammarManifest};

use rizz_ui::{
    RatatuiRenderer, Renderer, StateSnapshot, ThemeCell, Widget, WindowTree,
    panel::{Panel, PanelKind, PanelStack, Placement},
    precompute,
    render::{CursorStyle, GutterWidth, RenderedFrame},
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

/// Builder-style spec describing a popup to show. Built by the lisp
/// `popup-show` builtin from a widget value plus an options map. The
/// name field is filled in by the builtin from `popup-show`'s first arg.
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
    /// Lisp callable + width policy that renders the per-row gutter for every
    /// file buffer. `gutter_fn = None` = no gutter. Set by the lisp
    /// `set-gutter` builtin and consumed by the precompute pass. Width
    /// defaults to [`GutterWidth::Fit`].
    gutter_fn: Option<Rc<Value>>,
    gutter_width: GutterWidth,
    workdir: Rc<Path>,
    /// Directory holding `init.rz`. Stored so `reload-config` (and any
    /// future config-path query from lisp) can locate the script after init.
    config_dir: Rc<Path>,
    /// Runtime-registered tree-sitter grammars loaded from shared libraries,
    /// indexed by file extension. Populated by the `grammar-register` lisp
    /// builtin; consulted by [`Self::install_dynamic_highlighter`] when a
    /// buffer with a matching extension is opened.
    ts_registry: TsRegistry,
    /// Curated grammar manifest, seeded from `<config_dir>/grammars.toml` on
    /// first launch. Drives `(grammar-install)` lookups and the auto-load
    /// pass that registers a cached grammar on first buffer open.
    grammar_manifest: GrammarManifest,
    /// Names we've already surfaced a "grammar not installed" notify for, so
    /// opening many `.py` files doesn't spam the user with one popup per
    /// buffer. Cleared on `reload-config`.
    warned_missing_grammars: HashSet<Rc<str>>,
    /// Names we've already tried to auto-install in this session and which
    /// failed. Prevents retrying every time a new buffer of the same type is
    /// opened. Cleared on `reload-config` or a successful manual install.
    failed_auto_installs: HashSet<Rc<str>>,
    /// When true, opening a file whose extension matches a manifest entry but
    /// whose grammar is not yet cached triggers a one-shot `(grammar-install)`.
    /// Toggle via the `(set-grammar-auto-install)` lisp builtin. Default true.
    grammar_auto_install: bool,
    /// Notifications queued while `self.lisp` was checked out by an outer
    /// `with_lisp` call. Drained on the way out of that call so the user's
    /// `(notify …)` lisp fn still runs and produces the popup they expect,
    /// even when the originating Rust code (e.g. the auto-load hook in
    /// [`Self::install_highlighter`]) ran mid-lisp-eval.
    pending_notifications: Vec<String>,
    /// Vim-style named registers — fed by yank/delete/paste actions and the
    /// lisp `(register-*)` builtins. See [`rizz_registers`].
    registers: Registers,
    /// Vim's `"a` prefix: when set, the next yank/delete/paste targets this
    /// register name instead of the unnamed one. Cleared after the next
    /// consuming action so the next operation falls back to the defaults.
    pending_register: Option<char>,
    /// Last `/` pattern + direction + the overlays painted for the most
    /// recent search, so `n`/`N` can repeat and the next `/` can clear
    /// stale highlights.
    search: Search,
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
const GRAMMARS_MANIFEST_NAME: &str = "grammars.toml";
const EMBEDDED_GRAMMARS_MANIFEST: &str = include_str!("../../../grammars.toml");

/// Read `<config_dir>/<name>`, seeding it from `embedded` if missing so
/// first-run users land on a working file. Used for both `init.rz` and
/// `grammars.toml`.
fn load_or_seed(config_dir: &Path, name: &str, embedded: &str) -> anyhow::Result<String> {
    use std::fs;
    let path = config_dir.join(name);
    if !path.exists() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, embedded)?;
    }
    Ok(fs::read_to_string(&path)?)
}

/// Read `<config_dir>/init.rz`, seeding it from the embedded template if it
/// doesn't exist yet so first-run users land on a working config.
fn load_init_script(config_dir: &Path) -> anyhow::Result<String> {
    load_or_seed(config_dir, INIT_SCRIPT_NAME, EMBEDDED_INIT_SCRIPT)
}

/// Read `<config_dir>/grammars.toml`, seeding it from the embedded copy if
/// missing. A failure to read or parse falls back to an empty manifest with
/// a logged warning — a broken file should never keep the editor from boot.
fn load_grammar_manifest(config_dir: &Path) -> GrammarManifest {
    match load_or_seed(config_dir, GRAMMARS_MANIFEST_NAME, EMBEDDED_GRAMMARS_MANIFEST) {
        Ok(text) => match GrammarManifest::parse(&text) {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "grammars.toml parse failed — falling back to empty manifest");
                GrammarManifest::default()
            }
        },
        Err(e) => {
            warn!(error = %e, "grammars.toml load failed — falling back to empty manifest");
            GrammarManifest::default()
        }
    }
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
        let grammar_manifest = load_grammar_manifest(&config_dir);
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
            gutter_fn: None,
            gutter_width: GutterWidth::Fit,
            workdir: workdir.into(),
            config_dir: config_dir.into(),
            ts_registry: TsRegistry::new(),
            grammar_manifest,
            warned_missing_grammars: HashSet::new(),
            failed_auto_installs: HashSet::new(),
            grammar_auto_install: true,
            pending_notifications: Vec::new(),
            registers: Registers::new(),
            pending_register: None,
            search: Search::default(),
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
    /// runtime. On the way out, drain any notifications that were queued
    /// while the runtime was checked out (see [`Self::notify_via_lisp`]) —
    /// each one fires through the user's lisp `(notify …)` definition so the
    /// popup chrome stays under their control.
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
        self.drain_pending_notifications();
        result
    }

    /// Fire every queued notification through the lisp `(notify …)` fn now
    /// that `self.lisp` is owned again. A cap keeps a buggy `notify`
    /// definition from looping forever — anything past the cap falls back
    /// to the message journal so it's still recoverable via `:messages`.
    fn drain_pending_notifications(&mut self) {
        const MAX_DRAIN_PER_CALL: usize = 32;
        let mut drained = 0;
        while let Some(msg) = self.pending_notifications.pop() {
            if drained >= MAX_DRAIN_PER_CALL {
                warn!(
                    remaining = self.pending_notifications.len() + 1,
                    "notification drain cap hit — recording remainder to journal"
                );
                let remainder: Vec<String> =
                    std::iter::once(msg).chain(self.pending_notifications.drain(..)).collect();
                for m in remainder {
                    self.record_message(&m);
                }
                return;
            }
            drained += 1;
            self.notify_via_lisp(&msg);
        }
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

    /// Install the per-frame gutter callback. `width` is the column policy
    /// reserved on the left of every file buffer ([`GutterWidth::Fit`] sizes
    /// to the widest row, [`GutterWidth::Fixed`] reserves a constant count);
    /// pass `None` for `f` to disable the gutter entirely.
    pub fn set_gutter(&mut self, f: Option<Rc<Value>>, width: GutterWidth) {
        self.gutter_fn = f;
        self.gutter_width = width;
    }

    pub fn gutter_fn(&self) -> Option<&Rc<Value>> {
        self.gutter_fn.as_ref()
    }

    pub fn gutter_width(&self) -> GutterWidth {
        self.gutter_width
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
        self.registers.record_command(msg);
    }
    pub fn cmd_history(&self) -> impl Iterator<Item = &Rc<str>> {
        self.journal.commands()
    }

    /// Bridge from Rust-internal failure paths (eval errors, render-callback
    /// errors) to the lisp-side `notify` fn defined in `default.lisp`.
    ///
    /// Safe to call from inside a lisp builtin: when the runtime has already
    /// been checked out via `with_lisp`, the message is queued and drained
    /// on the way out of that call, so the user still gets their popup —
    /// just one tick later. Without the queue we'd either crash on a
    /// recursive `lisp.take()` or have to fall back to a silent
    /// `record_message`, which is invisible until the user opens `:messages`.
    pub fn notify_via_lisp(&mut self, msg: &str) {
        debug!(msg, "notify_via_lisp");
        if self.lisp.is_none() {
            debug!("notify_via_lisp queued — lisp runtime checked out");
            self.pending_notifications.push(msg.to_string());
            return;
        }
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
        if let Some(mut existing) = self.panels.remove_overlay_by_name(&name) {
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
            self.panels.push(existing);
            self.refresh_viewport();
            info!(?id, "overlay panel updated");
            return id;
        }
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
        let Some(panel) = self.panels.remove_overlay_by_name(name) else {
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
        let Some(panel) = self.panels.pop_top_overlay() else {
            trace!("no overlay to close");
            return false;
        };
        self.dispose_overlay_buf(panel.buf);
        self.refresh_viewport();
        true
    }

    fn dispose_overlay_buf(&mut self, removed: BufferId) {
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
    }

    pub fn top_popup_buf(&self) -> Option<BufferId> {
        self.panels.top_overlay().map(|p| p.buf)
    }

    /// Look up a named popup's backing buffer. Returns `None` if no popup
    /// with that name is currently on the stack.
    pub fn popup_buf_by_name(&self, name: &str) -> Option<BufferId> {
        self.panels.iter().find_map(|p| match &p.kind {
            PanelKind::Overlay { name: n, .. } if n.as_ref() == name => Some(p.buf),
            _ => None,
        })
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

    /// Declarative grammar install. Resolves the name against the curated
    /// manifest plus per-call opts, fetches the source (via `git`) and builds
    /// it (via the user's `tree-sitter` CLI) when no matching cache stamp is
    /// present, then registers the resulting library with [`Self::register_grammar`].
    /// Idempotent: a matching cache short-circuits the shell-outs.
    #[instrument(skip(self, opts), fields(name = name))]
    pub fn install_grammar(&mut self, name: &str, opts: InstallOpts) -> anyhow::Result<()> {
        let installed = rizz_ts_install::install(name, &opts, &self.grammar_manifest)
            .map_err(|e| anyhow::anyhow!(e))?;
        let highlights = rizz_ts_install::read_highlights(&installed)?;
        self.register_grammar(
            &installed.language,
            &installed.extensions,
            &installed.library,
            &highlights,
        )
        .map_err(|e| anyhow::anyhow!(e))?;
        // Clear any one-shot warning or failed-install marker we've recorded
        // for this grammar so a later uninstall+reinstall cycle can warn or
        // retry again if needed.
        let key = Rc::<str>::from(name);
        self.warned_missing_grammars.remove(&key);
        self.failed_auto_installs.remove(&key);
        Ok(())
    }

    /// True when the grammar cache holds a parser library + highlights query
    /// for `name`. Pure local check; never touches the network. Useful for
    /// `(if (not (grammar-installed? 'rust)) (grammar-install 'rust))`.
    pub fn grammar_installed(&self, name: &str) -> bool {
        rizz_ts_install::try_load_cached(name, &InstallOpts::default(), &self.grammar_manifest)
            .is_some()
    }

    /// True when opening a file with a known extension should auto-install
    /// the corresponding tree-sitter grammar. Toggled via the lisp
    /// `(set-grammar-auto-install …)` builtin.
    pub fn grammar_auto_install(&self) -> bool {
        self.grammar_auto_install
    }

    /// Set the auto-install flag. When toggled off, opening a file whose
    /// grammar is not yet cached reverts to the old behavior — a one-time
    /// notify pointing the user at `(grammar-install '<name>)`.
    pub fn set_grammar_auto_install(&mut self, on: bool) {
        self.grammar_auto_install = on;
    }

    /// If `buf` is a file buffer whose extension matches a registered
    /// dynamic grammar and no highlighter is currently attached, install one.
    /// A buffer that already has a (native) highlighter is left alone.
    ///
    /// When the extension is unknown to the registry but the curated manifest
    /// names a grammar for it, try to register it from the on-disk cache (no
    /// network). If the cache is empty and `grammar_auto_install` is set,
    /// shell out via [`Self::install_grammar`] to fetch and build it once.
    /// Otherwise surface a one-time notify pointing the user at
    /// `(grammar-install '<name>)`.
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
            return;
        }
        let Some(ext) = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
        else {
            return;
        };
        let Some(grammar_name) = self.grammar_manifest.grammar_for_ext(&ext).map(str::to_string)
        else {
            return;
        };
        if let Some(installed) = rizz_ts_install::try_load_cached(
            &grammar_name,
            &InstallOpts::default(),
            &self.grammar_manifest,
        ) {
            match rizz_ts_install::read_highlights(&installed) {
                Ok(highlights) => {
                    if let Err(e) = self.register_grammar(
                        &installed.language,
                        &installed.extensions,
                        &installed.library,
                        &highlights,
                    ) {
                        warn!(error = %e, name = grammar_name, "auto-load from cache failed");
                    } else if let Some(h) = self.ts_registry.highlighter_for_path(&path) {
                        self.bufs[buf].set_highlighter(Some(h));
                    }
                }
                Err(e) => warn!(error = %e, "could not read cached highlights"),
            }
            return;
        }
        let key: Rc<str> = Rc::from(grammar_name.as_str());
        if self.grammar_auto_install && !self.failed_auto_installs.contains(&key) {
            let msg = format!("installing tree-sitter grammar `{grammar_name}`…");
            self.notify_via_lisp(&msg);
            match self.install_grammar(&grammar_name, InstallOpts::default()) {
                Ok(()) => {
                    // `install_grammar` → `register_grammar` already loops
                    // over open buffers and attaches the new highlighter, so
                    // there's nothing left to do here.
                }
                Err(e) => {
                    self.failed_auto_installs.insert(key);
                    let msg = format!(
                        "auto-install of `{grammar_name}` failed: {e} — run `(grammar-install '{grammar_name})` manually or `(set-grammar-auto-install nil)` to silence this"
                    );
                    self.notify_via_lisp(&msg);
                }
            }
            return;
        }
        if self.warned_missing_grammars.insert(key) {
            let msg = format!(
                "grammar `{grammar_name}` not installed — run `(grammar-install '{grammar_name})` or `(set-grammar-auto-install t)`"
            );
            self.notify_via_lisp(&msg);
        }
    }

    pub fn pending_count_or_one(&self) -> u32 {
        self.count_prefix.or_one()
    }

    /// Read-only handle to the editor's vim-style registers. Lisp builtins
    /// use this to expose `(register-read ...)` / `(registers)` without
    /// owning a `&mut State`.
    pub fn registers(&self) -> &Registers {
        &self.registers
    }

    /// Mutable handle to the editor's vim-style registers. Used by the lisp
    /// `(register-write ...)` builtin and by tests.
    pub fn registers_mut(&mut self) -> &mut Registers {
        &mut self.registers
    }

    /// Register name the next yank/delete/paste should target — vim's `"a`
    /// prefix. `None` falls back to the unnamed register on the next
    /// consuming action.
    pub fn pending_register(&self) -> Option<char> {
        self.pending_register
    }

    /// Stage `name` as the next register to target. Cleared automatically by
    /// the next consuming action, but callers can also reset it explicitly by
    /// passing `None`.
    pub fn set_pending_register(&mut self, name: Option<char>) {
        self.pending_register = name;
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

    /// Insert pasted text as a single edit. The terminal sends OS-level
    /// pastes as one `Event::Paste` (bracketed paste must be enabled on the
    /// terminal); we bypass the keymap entirely so embedded newlines stay as
    /// newlines instead of being parsed as `Ctrl+J` keystrokes.
    #[instrument(skip(self, text), fields(len = text.len()))]
    pub fn handle_paste(&mut self, text: String) -> io::Result<()> {
        if !text.is_empty() {
            self.apply(&[Rc::new(Action::InsertMany(Rc::from(text)))])?;
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
            // Only mutating actions against the minibuffer text trigger a
            // refresh; arrows / `<esc>` / submit handle themselves.
            let edits_minibuffer_text = matches!(
                action.as_ref(),
                Action::InsertChar(_)
                    | Action::DeleteChar
                    | Action::DeleteCharAt(_)
                    | Action::InsertMany(_)
            );
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
                Action::ReplaceChar(c) => {
                    let count = self.count_prefix.or_one();
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, ch = %c, count, "Action::ReplaceChar");
                    self.bufs[f].replace_char_n(*c, count);
                }
                Action::OverwriteChar(c) => {
                    let f = self.focused_buf_id();
                    trace!(buf = ?f, ch = %c, "Action::OverwriteChar");
                    self.bufs[f].overwrite_char(*c);
                }
                Action::ReplaceBackspace => {
                    let f = self.focused_buf_id();
                    trace!(buf = ?f, "Action::ReplaceBackspace");
                    self.bufs[f].replace_backspace();
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
                    let yanked = self.bufs[f].yank_selection();
                    if self.bufs[f].delete_selection()
                        && let Some((text, kind)) = yanked
                    {
                        let name = self.pending_register.take();
                        self.registers.record_delete(text, kind, name);
                    }
                }
                Action::DeleteLine { count } => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, count, "Action::DeleteLine");
                    let yanked = self.bufs[f].yank_line(*count);
                    if self.bufs[f].delete_line(*count)
                        && let Some((text, kind)) = yanked
                    {
                        let name = self.pending_register.take();
                        self.registers.record_delete(text, kind, name);
                    }
                }
                Action::DeleteMotion { kind, count } => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, ?kind, count, "Action::DeleteMotion");
                    let yanked = self.bufs[f].yank_motion(*kind, *count);
                    if self.bufs[f].delete_motion(*kind, *count)
                        && let Some((text, kind)) = yanked
                    {
                        let name = self.pending_register.take();
                        self.registers.record_delete(text, kind, name);
                    }
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
                Action::GotoLastEdit { count } => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, count, "Action::GotoLastEdit");
                    self.bufs[f].goto_last_edit(*count);
                }
                Action::MoveCursor { kind, count } => {
                    let f = self.focused_buf_id();
                    trace!(buf = ?f, ?kind, count, "Action::MoveCursor");
                    self.bufs[f].move_cursor_n(*kind, *count);
                }
                Action::YankMotion { kind, count } => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, ?kind, count, "Action::YankMotion");
                    if let Some((text, k)) = self.bufs[f].yank_motion(*kind, *count) {
                        let name = self.pending_register.take();
                        self.registers.record_yank(text, k, name);
                    } else {
                        self.pending_register = None;
                    }
                }
                Action::YankLine { count } => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, count, "Action::YankLine");
                    if let Some((text, k)) = self.bufs[f].yank_line(*count) {
                        let name = self.pending_register.take();
                        self.registers.record_yank(text, k, name);
                    } else {
                        self.pending_register = None;
                    }
                }
                Action::YankSelection => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, "Action::YankSelection");
                    if let Some((text, k)) = self.bufs[f].yank_selection() {
                        let name = self.pending_register.take();
                        self.registers.record_yank(text, k, name);
                    } else {
                        self.pending_register = None;
                    }
                    self.bufs[f].set_mode(EditingMode::Normal);
                }
                Action::Paste { before, count } => {
                    let name = self.pending_register.take().unwrap_or('"');
                    let entry = self.registers.read(name).cloned();
                    let Some(entry) = entry else {
                        trace!(?name, "Action::Paste: empty register");
                        continue;
                    };
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, ?name, before, count, "Action::Paste");
                    // `count > 1` multiplies the text in one shot — matches
                    // vim's `Np`, where the inserted text is N copies of the
                    // register's payload (not N successive paste positions).
                    let n = (*count).max(1) as usize;
                    let entry = if n > 1 {
                        let mut joined = String::with_capacity(entry.text.len() * n);
                        for _ in 0..n {
                            joined.push_str(&entry.text);
                        }
                        RegisterEntry::new(joined, entry.kind)
                    } else {
                        entry
                    };
                    self.bufs[f].paste(&entry, *before);
                }
                Action::RegisterSelect(name) => {
                    debug!(name = ?name, "Action::RegisterSelect");
                    self.pending_register = Some(*name);
                }
                Action::RegisterSet { name, text, kind } => {
                    debug!(name = ?name, kind = ?kind, "Action::RegisterSet");
                    self.registers
                        .write(*name, RegisterEntry::new(text.clone(), *kind));
                }
                Action::YankTextObject {
                    object,
                    around,
                    count,
                } => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, ?object, around, count, "Action::YankTextObject");
                    if let Some((lo, hi, kind)) =
                        self.bufs[f].text_object_range(*object, *around, *count)
                    {
                        let text = self.bufs[f].rope().slice(lo..hi).to_string();
                        let name = self.pending_register.take();
                        self.registers.record_yank(text, kind, name);
                    } else {
                        self.pending_register = None;
                    }
                }
                Action::DeleteTextObject {
                    object,
                    around,
                    count,
                } => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, ?object, around, count, "Action::DeleteTextObject");
                    if let Some((lo, hi, kind)) =
                        self.bufs[f].text_object_range(*object, *around, *count)
                    {
                        let text = self.bufs[f].rope().slice(lo..hi).to_string();
                        if self.bufs[f].delete_range(lo, hi) {
                            let name = self.pending_register.take();
                            self.registers.record_delete(text, kind, name);
                        }
                    } else {
                        self.pending_register = None;
                    }
                }
                Action::SelectTextObject {
                    object,
                    around,
                    count,
                } => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, ?object, around, count, "Action::SelectTextObject");
                    if let Some((lo, hi, _)) =
                        self.bufs[f].text_object_range(*object, *around, *count)
                    {
                        self.bufs[f].select_char_range(lo, hi);
                    }
                }
                Action::CommandCancel => {
                    debug!("Action::CommandCancel");
                    self.exit_minibuffer();
                }
                Action::SearchSubmit => {
                    let pattern = self.bufs.minibuffer().text();
                    debug!(pattern, "Action::SearchSubmit");
                    if pattern.is_empty() {
                        // Empty submit → repeat last search forward from
                        // wherever live search left the cursor (vim's
                        // "/<enter>" semantic). The origin is no longer
                        // useful at this point.
                        self.search.take_origin();
                        self.exit_minibuffer();
                        if self.search.last_pattern().is_some() {
                            rizz_search::repeat_search(self, SearchDir::Forward);
                        }
                    } else {
                        // Live search already placed the cursor and painted
                        // overlays; just commit by recording the pattern in
                        // the `/` register and dropping the origin so cancel
                        // can't fire afterwards. Center the viewport on the
                        // match — vim's `nzz` flow, applied to the initial
                        // submit as well as `n`/`N` repeats.
                        self.search.take_origin();
                        self.registers.record_search(&*pattern);
                        self.exit_minibuffer();
                        let target_id = self.windows.focused_buf();
                        if let Some(b) = self.bufs.get_mut(target_id) {
                            b.move_cursor(rizz_text::MoveKind::Center);
                        }
                    }
                }
                Action::SearchCancel => {
                    debug!("Action::SearchCancel");
                    rizz_search::cancel_live_search(self);
                    self.exit_minibuffer();
                }
                Action::SearchNext => {
                    debug!("Action::SearchNext");
                    rizz_search::repeat_search(self, SearchDir::Forward);
                }
                Action::SearchPrev => {
                    debug!("Action::SearchPrev");
                    rizz_search::repeat_search(self, SearchDir::Backward);
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
            // Live-`/`-search: after any minibuffer-text-mutating action,
            // re-anchor the search at origin and re-run with the freshly
            // typed pattern. Skipped for the action arms that handle search
            // explicitly (Submit/Cancel/Next/Prev).
            if edits_minibuffer_text
                && self.bufs.minibuffer().mode() == EditingMode::Search
            {
                rizz_search::refresh_live_search(self);
            }
        }
        Ok(())
    }

    fn set_mode(&mut self, mode: EditingMode) {
        if matches!(mode, EditingMode::Command | EditingMode::Search) {
            debug!(?mode, "entering minibuffer-backed mode");
            // Stash the cursor + scroll position so cancel can restore it
            // and so live search has a stable origin to search from.
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
            // Only push a fresh minibuffer panel if one isn't already on the
            // stack — re-entering while already there is a no-op.
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
            gutter: self.gutter_fn.as_ref(),
            gutter_width: self.gutter_width,
            lisp_env: lisp.env(),
        });

        drop(_phase_guard);
        drop(_editor_guard);
        self.lisp = Some(lisp);

        result
    }
}

impl SearchHost for State {
    fn search(&self) -> &Search {
        &self.search
    }

    fn search_mut(&mut self) -> &mut Search {
        &mut self.search
    }

    fn focused_buf_id(&self) -> BufferId {
        // Prefer the buffer captured when `/` opened so that searches started
        // inside a popup keep targeting the popup buffer (rather than the
        // editor window underneath) for live updates and `n`/`N` repeats.
        // Falls back to the focused window when the recorded target is gone
        // — e.g. the popup was closed between submit and `n`.
        if let Some(id) = self.search.target_buf()
            && self.bufs.contains(id)
        {
            return id;
        }
        self.windows.focused_buf()
    }

    fn buf(&self, id: BufferId) -> Option<&Buffer> {
        self.bufs.get(id)
    }

    fn buf_mut(&mut self, id: BufferId) -> Option<&mut Buffer> {
        self.bufs.get_mut(id)
    }

    fn search_and_buf_mut(&mut self, id: BufferId) -> Option<(&mut Search, &mut Buffer)> {
        let buf = self.bufs.get_mut(id)?;
        Some((&mut self.search, buf))
    }

    fn minibuffer_text(&self) -> String {
        self.bufs.minibuffer().text()
    }

    fn notify(&mut self, msg: &str) {
        self.notify_via_lisp(msg);
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
    fn install_grammar_returns_helpful_error_for_unknown_name() {
        let mut s = test_state();
        let err = s
            .install_grammar("definitely-not-in-the-manifest", InstallOpts::default())
            .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("definitely-not-in-the-manifest"),
            "error should name the unknown grammar; got: {msg}"
        );
        assert!(s.ts_registry.is_empty(), "registry must stay empty");
    }

    #[test]
    fn grammar_installed_is_false_for_unknown() {
        let s = test_state();
        assert!(!s.grammar_installed("definitely-not-in-the-manifest"));
    }

    #[test]
    fn notify_via_lisp_queues_when_lisp_taken() {
        // `install_highlighter`'s auto-load path runs inside lisp actions
        // (`(edit "foo.rs")` → BufCreate → install_highlighter). At that
        // point `self.lisp` has been taken by `with_lisp`, so a re-entrant
        // `eval_lisp("(notify ...)")` would panic. Instead the message must
        // queue, and then fire through the user's `(notify …)` fn when the
        // outer `with_lisp` puts the runtime back.
        let mut s = test_state();
        let lisp = s.lisp.take();
        s.notify_via_lisp("queued notification");
        assert_eq!(
            s.pending_notifications.len(),
            1,
            "expected the message to be queued, not eval'd or dropped"
        );
        s.lisp = lisp;
        s.drain_pending_notifications();
        assert!(s.pending_notifications.is_empty(), "queue must be empty after drain");
        // The user's `(notify …)` runs `notify-record`, which appends to the
        // message history — so a successful drain leaves the message there.
        let found = s
            .message_history()
            .any(|m| m.as_ref() == "queued notification");
        assert!(found, "drain should have routed the message through `(notify …)`");
    }

    #[test]
    fn with_lisp_drains_queued_notifications() {
        // End-to-end: anything that queues during the body of a `with_lisp`
        // call fires through `(notify …)` on the way out.
        let mut s = test_state();
        let r: Result<_, RizzError> = s.with_lisp(|_| {
            // Inside the closure `self.lisp` is None — simulate the auto-load
            // path by reaching back through the editor bridge.
            crate::lisp::with_editor_mut(|st| st.notify_via_lisp("drained via with_lisp"));
            Ok(())
        });
        r.unwrap();
        assert!(s.pending_notifications.is_empty());
        let found = s
            .message_history()
            .any(|m| m.as_ref() == "drained via with_lisp");
        assert!(found, "with_lisp must drain queued notifications on exit");
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

    // ---- registers ----------------------------------------------------

    fn reg_text(s: &State, name: char) -> Option<String> {
        s.registers().read(name).map(|e| e.text.to_string())
    }

    #[test]
    fn yank_line_fills_unnamed_and_zero() {
        let mut s = test_state();
        let b = primary(&s);
        s.bufs[b].clear_with("hello\nworld\n");
        s.apply(&[Rc::new(Action::YankLine { count: 1 })]).unwrap();
        assert_eq!(reg_text(&s, '"').as_deref(), Some("hello\n"));
        assert_eq!(reg_text(&s, '0').as_deref(), Some("hello\n"));
        // numbered 1-9 stay untouched on yank
        assert!(s.registers().read('1').is_none());
    }

    #[test]
    fn delete_line_rotates_numbered_register() {
        let mut s = test_state();
        let b = primary(&s);
        s.bufs[b].clear_with("a\nb\nc\nd\n");
        s.apply(&[Rc::new(Action::DeleteLine { count: 1 })])
            .unwrap();
        assert_eq!(reg_text(&s, '1').as_deref(), Some("a\n"));
        s.apply(&[Rc::new(Action::DeleteLine { count: 1 })])
            .unwrap();
        assert_eq!(reg_text(&s, '1').as_deref(), Some("b\n"));
        assert_eq!(reg_text(&s, '2').as_deref(), Some("a\n"));
        // delete never fills the yank register
        assert!(s.registers().read('0').is_none());
    }

    #[test]
    fn yank_then_paste_after_inserts_below() {
        let mut s = test_state();
        let b = primary(&s);
        s.bufs[b].clear_with("hello\nworld\n");
        s.apply(&[Rc::new(Action::YankLine { count: 1 })]).unwrap();
        // move to second line
        s.bufs[b].move_cursor_n(rizz_text::MoveKind::Relative(Position::new(0, 1)), 1);
        s.apply(&[Rc::new(Action::Paste {
            before: false,
            count: 1,
        })])
        .unwrap();
        assert_eq!(s.bufs[b].text(), "hello\nworld\nhello\n");
    }

    #[test]
    fn paste_count_repeats_entry() {
        let mut s = test_state();
        let b = primary(&s);
        s.bufs[b].clear_with("abc");
        // seed the unnamed register without going through delete/yank
        s.registers_mut().write('"', RegisterEntry::charwise("X"));
        s.apply(&[Rc::new(Action::Paste {
            before: false,
            count: 3,
        })])
        .unwrap();
        assert_eq!(s.bufs[b].text(), "aXXXbc");
    }

    #[test]
    fn register_select_targets_named_register() {
        let mut s = test_state();
        let b = primary(&s);
        s.bufs[b].clear_with("alpha\nbeta\n");
        s.apply(&[
            Rc::new(Action::RegisterSelect('a')),
            Rc::new(Action::YankLine { count: 1 }),
        ])
        .unwrap();
        assert_eq!(reg_text(&s, 'a').as_deref(), Some("alpha\n"));
        // pending register is cleared after a consuming action
        assert!(s.pending_register().is_none());
        // and the unnamed register also got the same text
        assert_eq!(reg_text(&s, '"').as_deref(), Some("alpha\n"));
    }

    #[test]
    fn paste_from_named_register() {
        let mut s = test_state();
        let b = primary(&s);
        s.bufs[b].clear_with("abc");
        s.registers_mut().write('a', RegisterEntry::charwise("ZZ"));
        s.apply(&[
            Rc::new(Action::RegisterSelect('a')),
            Rc::new(Action::Paste {
                before: false,
                count: 1,
            }),
        ])
        .unwrap();
        assert_eq!(s.bufs[b].text(), "aZZbc");
    }

    #[test]
    fn delete_selection_fills_unnamed() {
        let mut s = test_state();
        let b = primary(&s);
        s.bufs[b].clear_with("hello");
        s.bufs[b].set_mode(EditingMode::Visual);
        s.bufs[b].move_cursor_n(rizz_text::MoveKind::Relative(Position::new(2, 0)), 1);
        s.apply(&[Rc::new(Action::DeleteSelection)]).unwrap();
        assert_eq!(s.bufs[b].text(), "lo");
        assert_eq!(reg_text(&s, '"').as_deref(), Some("hel"));
        assert_eq!(reg_text(&s, '-').as_deref(), Some("hel"));
    }

    #[test]
    fn yank_selection_returns_to_normal() {
        let mut s = test_state();
        let b = primary(&s);
        s.bufs[b].clear_with("hello");
        s.bufs[b].set_mode(EditingMode::Visual);
        s.bufs[b].move_cursor_n(rizz_text::MoveKind::Relative(Position::new(2, 0)), 1);
        s.apply(&[Rc::new(Action::YankSelection)]).unwrap();
        assert_eq!(reg_text(&s, '"').as_deref(), Some("hel"));
        assert_eq!(s.bufs[b].mode(), EditingMode::Normal);
        // buffer text is unchanged by yank
        assert_eq!(s.bufs[b].text(), "hello");
    }

    #[test]
    fn lisp_register_read_and_write_round_trip() {
        let mut s = test_state();
        s.eval_lisp(r#"(register-write "a" "hello")"#).unwrap();
        let v = s.eval_lisp(r#"(register-read "a")"#).unwrap();
        assert_eq!(v.display().to_string(), "hello");
    }

    #[test]
    fn lisp_yank_then_paste_charwise() {
        let mut s = test_state();
        let b = primary(&s);
        s.bufs[b].clear_with("hello world");
        s.eval_lisp("(yank-motion 'word-forward)").unwrap();
        assert_eq!(reg_text(&s, '"').as_deref(), Some("hello "));
        // paste-before so the inserted text lands at the cursor (col 0)
        s.eval_lisp("(paste-before)").unwrap();
        assert_eq!(s.bufs[b].text(), "hello hello world");
    }

    // ---- text objects -------------------------------------------------

    #[test]
    fn delete_inner_word_under_cursor() {
        use rizz_text::TextObject;
        let mut s = test_state();
        let b = primary(&s);
        s.bufs[b].clear_with("hello world");
        s.bufs[b].move_cursor_n(rizz_text::MoveKind::Relative(Position::new(2, 0)), 1);
        s.apply(&[Rc::new(Action::DeleteTextObject {
            object: TextObject::Word,
            around: false,
            count: 1,
        })])
        .unwrap();
        assert_eq!(s.bufs[b].text(), " world");
        assert_eq!(reg_text(&s, '"').as_deref(), Some("hello"));
    }

    #[test]
    fn yank_around_paren_block_includes_brackets() {
        use rizz_text::TextObject;
        let mut s = test_state();
        let b = primary(&s);
        s.bufs[b].clear_with("foo(bar)baz");
        s.bufs[b].move_cursor_n(rizz_text::MoveKind::Relative(Position::new(5, 0)), 1);
        s.apply(&[Rc::new(Action::YankTextObject {
            object: TextObject::Paren,
            around: true,
            count: 1,
        })])
        .unwrap();
        assert_eq!(reg_text(&s, '"').as_deref(), Some("(bar)"));
        // buffer text is unchanged
        assert_eq!(s.bufs[b].text(), "foo(bar)baz");
    }

    #[test]
    fn select_inner_dquote_drops_into_visual() {
        use rizz_text::TextObject;
        let mut s = test_state();
        let b = primary(&s);
        s.bufs[b].clear_with(r#"x "hello" y"#);
        s.bufs[b].move_cursor_n(rizz_text::MoveKind::Relative(Position::new(5, 0)), 1);
        s.apply(&[Rc::new(Action::SelectTextObject {
            object: TextObject::DoubleQuote,
            around: false,
            count: 1,
        })])
        .unwrap();
        assert_eq!(s.bufs[b].mode(), EditingMode::Visual);
        assert_eq!(s.bufs[b].selected_text().as_deref(), Some("hello"));
    }

    #[test]
    fn lisp_delete_inner_word_works() {
        let mut s = test_state();
        let b = primary(&s);
        s.bufs[b].clear_with("hello world");
        s.bufs[b].move_cursor_n(rizz_text::MoveKind::Relative(Position::new(2, 0)), 1);
        s.eval_lisp(r#"(delete-inner "word")"#).unwrap();
        assert_eq!(s.bufs[b].text(), " world");
    }

    #[test]
    fn lisp_yank_around_paren_works() {
        let mut s = test_state();
        let b = primary(&s);
        s.bufs[b].clear_with("foo(bar)");
        s.bufs[b].move_cursor_n(rizz_text::MoveKind::Relative(Position::new(5, 0)), 1);
        s.eval_lisp(r#"(yank-around "paren")"#).unwrap();
        assert_eq!(reg_text(&s, '"').as_deref(), Some("(bar)"));
    }

    #[test]
    fn lisp_select_inner_paren_visual_mode() {
        let mut s = test_state();
        let b = primary(&s);
        s.bufs[b].clear_with("foo(bar)");
        s.bufs[b].move_cursor_n(rizz_text::MoveKind::Relative(Position::new(5, 0)), 1);
        s.eval_lisp(r#"(select-inner "paren")"#).unwrap();
        assert_eq!(s.bufs[b].selected_text().as_deref(), Some("bar"));
    }

    // ---- vim `r` / `R` ------------------------------------------------

    #[test]
    fn r_chord_replaces_char_under_cursor() {
        use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
        let mut s = test_state();
        let b = primary(&s);
        s.bufs[b].clear_with("hello");
        s.handle_key_event(CT::new(KeyCode::Char('r'), KeyModifiers::NONE))
            .unwrap();
        // Chord descended; nothing changed yet.
        assert_eq!(s.bufs[b].text(), "hello");
        s.handle_key_event(CT::new(KeyCode::Char('X'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(s.bufs[b].text(), "Xello");
        assert_eq!(s.bufs[b].mode(), EditingMode::Normal);
    }

    #[test]
    fn count_prefix_scales_r_chord() {
        use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
        let mut s = test_state();
        let b = primary(&s);
        s.bufs[b].clear_with("hello");
        s.handle_key_event(CT::new(KeyCode::Char('3'), KeyModifiers::NONE))
            .unwrap();
        s.handle_key_event(CT::new(KeyCode::Char('r'), KeyModifiers::NONE))
            .unwrap();
        s.handle_key_event(CT::new(KeyCode::Char('z'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(s.bufs[b].text(), "zzzlo");
    }

    #[test]
    fn capital_r_enters_replace_mode_and_overwrites() {
        use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
        let mut s = test_state();
        let b = primary(&s);
        s.bufs[b].clear_with("hello");
        s.handle_key_event(CT::new(KeyCode::Char('R'), KeyModifiers::SHIFT))
            .unwrap();
        assert_eq!(s.bufs[b].mode(), EditingMode::Replace);
        s.handle_key_event(CT::new(KeyCode::Char('H'), KeyModifiers::SHIFT))
            .unwrap();
        s.handle_key_event(CT::new(KeyCode::Char('I'), KeyModifiers::SHIFT))
            .unwrap();
        assert_eq!(s.bufs[b].text(), "HIllo");
        s.handle_key_event(CT::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(s.bufs[b].mode(), EditingMode::Normal);
    }

    #[test]
    fn replace_mode_at_eol_extends_line() {
        use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
        let mut s = test_state();
        let b = primary(&s);
        s.bufs[b].clear_with("hi");
        // Park the cursor past the last char (`A` semantics) before
        // entering Replace mode: subsequent overwrites have to fall back
        // to insert because there's no char under the cursor to replace.
        s.bufs[b].set_mode(EditingMode::Insert);
        s.bufs[b].move_cursor_n(rizz_text::MoveKind::LineEnd, 1);
        s.bufs[b].set_mode(EditingMode::Replace);
        s.handle_key_event(CT::new(KeyCode::Char('!'), KeyModifiers::NONE))
            .unwrap();
        s.handle_key_event(CT::new(KeyCode::Char('?'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(s.bufs[b].text(), "hi!?");
    }

    #[test]
    fn replace_mode_backspace_restores_original_chars() {
        use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
        let mut s = test_state();
        let b = primary(&s);
        s.bufs[b].clear_with("hello");
        s.handle_key_event(CT::new(KeyCode::Char('R'), KeyModifiers::SHIFT))
            .unwrap();
        s.handle_key_event(CT::new(KeyCode::Char('H'), KeyModifiers::SHIFT))
            .unwrap();
        s.handle_key_event(CT::new(KeyCode::Char('I'), KeyModifiers::SHIFT))
            .unwrap();
        assert_eq!(s.bufs[b].text(), "HIllo");
        s.handle_key_event(CT::new(KeyCode::Backspace, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(s.bufs[b].text(), "Hello");
        s.handle_key_event(CT::new(KeyCode::Backspace, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(s.bufs[b].text(), "hello");
        // Past the session start: keymap-level no-op (cursor doesn't move,
        // buffer doesn't change).
        s.handle_key_event(CT::new(KeyCode::Backspace, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(s.bufs[b].text(), "hello");
        s.handle_key_event(CT::new(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap();
        // Nothing was committed — there's no edit to undo.
        assert!(!s.bufs[b].undo());
    }
}
