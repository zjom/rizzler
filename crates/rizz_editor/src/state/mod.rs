//! The editor process — single owner of every long-lived editor field.
//!
//! `State::apply` is the only mutator: every key event, lisp call, or
//! external trigger ultimately produces an [`rizz_actions::Action`] list and
//! sends it through here. The single-funnel invariant is what makes undo,
//! scripting, and tests tractable.
//!
//! # Layout
//!
//! `State` originally lived in one 3,300-line file. It's now split by concern:
//!
//! - [`buffers`] — buffer list / window / file open-write-delete / cycle
//! - [`surface`] — popups, panel stack, viewport sizing, mode switching
//! - [`input`] — keymap resolution, key event ring, count prefix
//! - [`render`] — renderer, theme, frame/gutter callbacks, render pass
//! - [`scripting`] — lisp runtime + notifications + journal hooks
//! - [`workspace`] — workdir / config dir / init script / manifest seeding
//! - [`lang`] — tree-sitter and LSP install / attach (duplicated; PR2 dedupes)
//! - [`lsp_session`] — in-flight LSP requests + callbacks + per-tick drain
//! - [`apply`] — the single-funnel `Action::apply` match
//!
//! `State`'s fields are private; all child modules can see them because they
//! are descendants of this module.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use rizz::runtime::Value;
use rizz_ringbuffer::RingBuffer;
use tracing::info;

use rizz_actions::KeymapRegistry;
use rizz_core::EditingMode;
use rizz_input::{CountPrefix, KeyEvent};
use rizz_lsp::{LspRegistry, RequestSeq};
use rizz_lsp_install::Manifest as LspManifest;
use rizz_registers::Registers;
use rizz_search::{Search, SearchHost};
use rizz_text::{Buffer, BufferId, WrapMode, io as buffer_io};
use rizz_ts::TsRegistry;
use rizz_ts_install::Manifest as GrammarManifest;
use rizz_ui::{
    RatatuiRenderer, Renderer, ThemeCell, Widget, WindowTree,
    panel::{PanelStack, Placement},
    render::GutterWidth,
};

use crate::buffer_list::BufferList;
use crate::journal::Journal;
use crate::lisp::LispRuntime;

pub use rizz_core::{FocusDir, SplitDir};

mod apply;
mod buffers;
mod input;
mod lang;
mod lsp_session;
mod render;
mod scripting;
mod surface;
mod workspace;

#[cfg(test)]
mod tests;

use lsp_session::{PendingCodeActions, PendingCompletion, PendingLspKind};
use workspace::{default_config_dir, load_grammar_manifest, load_lsp_manifest, resolve_workdir};

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
    /// indexed by file extension.
    ts_registry: TsRegistry,
    /// Curated grammar manifest, seeded from `<config_dir>/grammars.toml` on
    /// first launch.
    grammar_manifest: GrammarManifest,
    /// Names we've already surfaced a "grammar not installed" notify for, so
    /// opening many `.py` files doesn't spam the user with one popup per
    /// buffer. Cleared on `reload-config`.
    warned_missing_grammars: HashSet<Rc<str>>,
    /// Names we've already tried to auto-install in this session and which
    /// failed. Prevents retrying every time a new buffer of the same type is
    /// opened.
    failed_auto_installs: HashSet<Rc<str>>,
    /// When true, opening a file whose extension matches a manifest entry but
    /// whose grammar is not yet cached triggers a one-shot `(grammar-install)`.
    grammar_auto_install: bool,
    /// Notifications queued while `self.lisp` was checked out by an outer
    /// `with_lisp` call. Drained on the way out of that call.
    pending_notifications: Vec<String>,
    /// Vim-style named registers — fed by yank/delete/paste actions and the
    /// lisp `(register-*)` builtins.
    registers: Registers,
    /// Vim's `"a` prefix: when set, the next yank/delete/paste targets this
    /// register name instead of the unnamed one.
    pending_register: Option<char>,
    /// Last `/` pattern + direction + the overlays painted for the most
    /// recent search.
    search: Search,
    /// Editor-side handle to spawned LSP clients, indexed by symbolic name.
    lsp_registry: LspRegistry,
    /// Curated LSP server manifest, seeded from `<config_dir>/lsp.toml` on
    /// first launch.
    lsp_manifest: LspManifest,
    /// When true, opening a file whose extension matches a manifest entry
    /// but whose binary is missing from PATH triggers a one-shot install
    /// recipe.
    lsp_auto_install: bool,
    /// Names we've already surfaced a "no lsp server" notify for.
    warned_missing_servers: HashSet<Rc<str>>,
    /// Names whose auto-install we already tried and failed.
    failed_lsp_auto_installs: HashSet<Rc<str>>,
    /// In-flight LSP requests: maps sequence id → what to do with the
    /// response when it arrives.
    pending_lsp_requests: HashMap<RequestSeq, PendingLspKind>,
    /// `uri → buffer id` so server-pushed notifications (diagnostics,
    /// applyEdit, …) can find the right buffer.
    buf_by_uri: HashMap<String, BufferId>,
    /// Monotonic sequence counter for LSP request routing.
    next_lsp_seq: RequestSeq,
    /// Lisp callable invoked with the items array + anchor when a
    /// `textDocument/completion` response arrives.
    lsp_completion_fn: Option<Rc<Value>>,
    /// Lisp callable invoked with the actions array when a
    /// `textDocument/codeAction` response arrives.
    lsp_code_action_fn: Option<Rc<Value>>,
    /// The most recently surfaced completion batch. Keyed by id so lisp can
    /// invoke an item without round-tripping the insert-text back to Rust.
    lsp_pending_completion: Option<PendingCompletion>,
    /// The most recently surfaced code-action batch.
    lsp_pending_code_actions: Option<PendingCodeActions>,
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
        let lsp_manifest = load_lsp_manifest(&config_dir);
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
            lsp_registry: LspRegistry::new(),
            lsp_manifest,
            lsp_auto_install: true,
            warned_missing_servers: HashSet::new(),
            failed_lsp_auto_installs: HashSet::new(),
            pending_lsp_requests: HashMap::new(),
            buf_by_uri: HashMap::new(),
            next_lsp_seq: 1,
            lsp_completion_fn: None,
            lsp_code_action_fn: None,
            lsp_pending_completion: None,
            lsp_pending_code_actions: None,
        };
        if let Some(path) = config.edit_path
            && !path.is_dir()
        {
            state.bufs[first_file] = buffer_io::with_path(Rc::<Path>::from(path));
            state.install_highlighter(first_file);
            state.install_lsp_client(first_file);
        }
        state.refresh_viewport();

        state.run_init_script()?;

        Ok(state)
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

    pub fn quit_requested(&self) -> bool {
        self.quit
    }

    pub fn registers(&self) -> &Registers {
        &self.registers
    }

    pub fn registers_mut(&mut self) -> &mut Registers {
        &mut self.registers
    }

    /// Register the next yank/delete/paste should target — vim's `"a` prefix.
    /// `None` falls back to the unnamed register.
    pub fn pending_register(&self) -> Option<char> {
        self.pending_register
    }

    pub fn set_pending_register(&mut self, name: Option<char>) {
        self.pending_register = name;
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
        // Searches started inside a popup must keep targeting the popup
        // buffer even after focus moves. Falls back to the focused window
        // when the recorded target is gone (e.g. popup closed before `n`).
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

/// Test support — `pub` so the lisp module's tests can use it.
pub mod test_support {
    use super::*;
    use rizz_ui::render::{RenderedFrame, Renderer, StateSnapshot};

    /// A renderer that does nothing — for tests that don't want to touch a terminal.
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
