//! The editor process — single owner of every long-lived editor field.
//!
//! `State::apply` is the only mutator: every key event, lisp call, or
//! external trigger ultimately produces an [`rizz_actions::Action`] list and
//! sends it through here. The single-funnel invariant is what makes undo,
//! scripting, and tests tractable.
//!
//! # Layout
//!
//! `State` itself is a thin facade. Each long-lived concern lives in its own
//! subsystem struct; cross-cutting methods stay on `impl State` (split across
//! these files) but they reach into one subsystem at a time:
//!
//! - [`buffers`] — buffer list, focused-buffer accessors, file open/edit/
//!   write/delete, window split/close, `:bn`/`:bp` cycle
//! - [`surface`] — `Surface` (windows + panel stack); popup show/hide,
//!   viewport sizing, minibuffer mode switching
//! - [`input`] — `Input` (keymap, key-event ring, count prefix);
//!   `handle_key_event` resolution
//! - [`render`] — `Render` (renderer, theme, frame/gutter callbacks);
//!   per-tick render pass
//! - [`scripting`] — `Scripting` (lisp runtime + notification queue);
//!   `with_lisp` checkout, eval / notify
//! - [`workspace`] — `Workspace` (workdir + config dir); init script,
//!   manifest seeding, URI conversion
//! - [`lang`] — `LangIntegration` (two `LanguageBackend` instances + runtime
//!   registries); install / attach for tree-sitter and LSP
//! - [`lsp_session`] — `LspSession` (pending requests, sequence counter,
//!   completion / code-action callbacks) + the per-tick event drain
//! - [`lsp_requests`] — outgoing `lsp_send_*` request senders
//! - [`lsp_responses`] — response display (`show_lsp_*`) + edit application
//! - [`apply`] — the single-funnel `Action::apply` dispatch table
//!
//! `State`'s fields are private; all child modules can see them because they
//! are descendants of this module.

use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use tracing::info;

use rizz_core::EditingMode;
use rizz_registers::Registers;
use rizz_search::{Search, SearchHost};
use rizz_text::{Buffer, BufferId, WrapMode, io as buffer_io};
use rizz_ui::{RatatuiRenderer, Renderer, Widget, panel::Placement};

use crate::buffer_list::BufferList;
use crate::journal::Journal;

pub use rizz_core::{FocusDir, SplitDir};

mod apply;
mod buffers;
mod input;
mod lang;
mod lsp_requests;
mod lsp_responses;
mod lsp_session;
mod render;
mod scripting;
mod surface;
mod workspace;

#[cfg(test)]
mod tests;

use input::Input;
use lang::LangIntegration;
use lsp_session::LspSession;
use render::Render;
use scripting::Scripting;
use surface::Surface;
use workspace::{
    Workspace, default_config_dir, load_grammar_manifest, load_lsp_manifest, resolve_workdir,
};

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

/// Cursor into command history for `<up>`/`<down>` recall in command mode.
/// Reset every time command mode is (re)entered.
#[derive(Default)]
struct CmdHistoryNav {
    /// Index (oldest = 0) of the recalled command currently shown, or `None`
    /// when the user is editing their own freshly-typed line.
    pos: Option<usize>,
    /// The line the user had typed before recall began, so `<down>` past the
    /// newest entry can restore it.
    draft: String,
}

pub struct State {
    bufs: BufferList,
    journal: Journal,
    /// Editor window tree + the panel stack stacked above it (minibuffer +
    /// popups). When the panel stack is empty, focus falls through to the
    /// focused window leaf. See [`surface::Surface`].
    surface: Surface,
    quit: bool,
    /// Keymap dispatch + the inputs feeding it (key event ring, count
    /// prefix, chord timeout). See [`input::Input`].
    input: Input,
    /// Rendering plumbing: terminal renderer, theme, frame + gutter
    /// callbacks. See [`render::Render`].
    render: Render,
    /// Embedded lisp runtime + the notification queue that buffers messages
    /// emitted while the runtime is checked out by `with_lisp`. See
    /// [`scripting::Scripting`].
    scripting: Scripting,
    /// Filesystem roots: workdir (relative-path base) + config dir (where
    /// `init.rz` / `grammars.toml` / `lsp.toml` live). See
    /// [`workspace::Workspace`].
    workspace: Workspace,
    /// Tree-sitter + LSP install state, grouped behind one struct. Each
    /// half holds a [`rizz_install::LanguageBackend`] (manifest + auto-install
    /// flag + warn / failed-install sets) plus its runtime registry handle.
    lang: LangIntegration,
    /// Vim-style named registers — fed by yank/delete/paste actions and the
    /// lisp `(register-*)` builtins.
    registers: Registers,
    /// Vim's `"a` prefix: when set, the next yank/delete/paste targets this
    /// register name instead of the unnamed one.
    pending_register: Option<char>,
    /// Last `/` pattern + direction + the overlays painted for the most
    /// recent search.
    search: Search,
    /// In-flight LSP request bookkeeping (pending request map, sequence
    /// counter, response callbacks, last completion + code-action batch).
    /// See [`lsp_session::LspSession`].
    lsp_session: LspSession,
    /// `<up>`/`<down>` recall position in command history. See
    /// [`CmdHistoryNav`].
    cmd_history_nav: CmdHistoryNav,
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
            journal: Journal::new(),
            surface: Surface::new(first_file),
            quit: false,
            input: Input::new(config.keycombo_timeout),
            render: Render::new(config.renderer),
            scripting: Scripting::new(),
            workspace: Workspace::new(workdir.into(), config_dir.into()),
            lang: LangIntegration::new(grammar_manifest, lsp_manifest),
            registers: Registers::new(),
            pending_register: None,
            search: Search::default(),
            lsp_session: LspSession::new(),
            cmd_history_nav: CmdHistoryNav::default(),
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
        match self.surface.panels.top_buf() {
            Some(id) => id,
            None => self.surface.windows.focused_buf(),
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
        self.surface.windows.focused_buf()
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

    /// [`test_state`] with `text` loaded into the primary file buffer —
    /// the dominant fixture shape across the state tests.
    pub fn test_state_with_text(text: &str) -> State {
        let mut s = test_state();
        let b = s.bufs.first_file_buf();
        s.bufs[b].clear_with(text);
        s
    }
}
