//! The closed enum of every behaviour the editor can be asked to perform.
//!
//! Every input source ultimately produces an [`Action`], and
//! `rizz_editor::State::apply` is the single point that interprets them — so
//! adding new behaviour means adding a variant here and a match arm in
//! `apply`. Resist the urge to grow new entry points; the single-funnel
//! invariant is load-bearing for undo, scripting, and tests.
//!
//! See `docs/ARCHITECTURE.md` at the repo root for the full subsystem
//! layout and a keystroke-to-buffer-mutation trace.

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use rizz::runtime::Value;
use rizz_core::{EditingMode, FocusDir, Position, SplitDir};
use rizz_input::KeyEvent;
use rizz_registers::RegisterKind;
use rizz_text::{BufferId, MoveKind, TextObject};

use crate::lsp::{
    CodeActionOwned, CommandOwned, CompletionItemOwned, LocationOwned, LspClientId,
    TextEditOwned, WorkspaceEditOwned,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Action {
    Noop,
    Quit,
    SetMode(EditingMode),

    InsertChar(char),
    /// Vim `r<char>` — replace the `count` chars at the cursor with `c` as a
    /// single tracked edit. The cursor lands on the last replaced char.
    /// Count comes from the pending count prefix at apply time, since the
    /// action is emitted by the keymap's `on_char` after `r` descends.
    ReplaceChar(char),
    /// Vim Replace-mode keystroke — overwrite the char under the cursor with
    /// `c` and advance. At end-of-line the char is inserted (extends the
    /// line). Buffered into the buffer's in-flight Replace-mode session
    /// and committed as one delta on exit.
    OverwriteChar(char),
    /// Vim Replace-mode `<bs>` — walk back over the last `OverwriteChar`,
    /// restoring the original character if the slot was overwritten or
    /// deleting the char if it was an extension. No-op past the start of
    /// the session.
    ReplaceBackspace,
    /// Insert `char` speculatively while a chord prefix is in flight. The
    /// keymap emits this on `Descend` when the descending mode's `on_char`
    /// would have produced an `InsertChar`, so the user sees their typing
    /// immediately. Resolves to either `CommitSpeculation` (chord aborts)
    /// or `RollbackSpeculation` (chord completes).
    SpeculativeInsertChar(char),
    /// Promote the focused buffer's pending speculative inserts to a single
    /// tracked delta. Emitted by the keymap on chord abort.
    CommitSpeculation,
    /// Discard the focused buffer's pending speculative inserts (rope and
    /// cursor unwind; no entry in undo history). Emitted by the keymap on
    /// chord completion so the staged text disappears before the chord's
    /// own action runs.
    RollbackSpeculation,
    /// Insert a whole string at the cursor as a single undo step.
    InsertMany(Rc<str>),
    InsertNewline,
    DeleteChar,
    DeleteCharAt(Position<usize>),
    /// Remove the focused buffer's current visual selection and return to
    /// Normal mode. No-op when the buffer is not in a visual mode.
    DeleteSelection,
    /// Vim `dd` — delete `count` whole lines starting at the cursor.
    DeleteLine {
        count: u32,
    },
    /// Vim `d<motion>` — delete from the cursor to the destination of
    /// running `kind` `count` times. Vertical / file-jump motions delete
    /// whole lines; everything else deletes a character range.
    DeleteMotion {
        kind: MoveKind,
        count: u32,
    },
    Undo,
    Redo,
    /// Vim `g;` — jump to the position of the last edit. Repeated calls walk
    /// further back through the buffer's change tree. `count` takes that
    /// many steps in one go.
    GotoLastEdit {
        count: u32,
    },
    /// Move the cursor by `kind`, repeated `count` times (0/1 == once).
    MoveCursor {
        kind: MoveKind,
        count: u32,
    },

    /// Vim `y<motion>` — yank the spanned text into the registers without
    /// modifying the buffer. Linewise / charwise tagging mirrors
    /// `DeleteMotion`.
    YankMotion {
        kind: MoveKind,
        count: u32,
    },
    /// Vim `yy` / `Nyy` — linewise yank of `count` lines starting at the
    /// cursor row.
    YankLine {
        count: u32,
    },
    /// Vim `y` in a visual mode — yank the selection, then drop back to
    /// Normal mode (mirrors `DeleteSelection`'s mode handling).
    YankSelection,
    /// Vim `p` (`before=false`) / `P` (`before=true`) — paste from the
    /// active register. `count` copies the entry that many times.
    Paste {
        before: bool,
        count: u32,
    },
    /// Vim `"a` prefix — stage `name` as the register the next yank, delete,
    /// or paste should target. Cleared after the next consuming action.
    RegisterSelect(char),
    /// Write `text` into register `name` directly (used by lisp + tests).
    /// `A`-`Z` follows the usual append semantics.
    RegisterSet {
        name: char,
        text: Rc<str>,
        kind: RegisterKind,
    },

    /// Vim `d{i,a}<obj>` — delete the range a text object resolves to. The
    /// captured text feeds the same register routing as `DeleteMotion`.
    DeleteTextObject {
        object: TextObject,
        around: bool,
        count: u32,
    },
    /// Vim `y{i,a}<obj>` — yank the range a text object resolves to.
    YankTextObject {
        object: TextObject,
        around: bool,
        count: u32,
    },
    /// Vim `v{i,a}<obj>` — switch into Visual mode with the text object's
    /// range pre-selected. Anchor lands at the range's start; the cursor
    /// lands on its last char.
    SelectTextObject {
        object: TextObject,
        around: bool,
        count: u32,
    },

    CommandCancel,

    /// Vim `/` — read the minibuffer text as a regex, find every match in
    /// the focused buffer, highlight them, and jump the cursor to the first
    /// match at or after the current position. Wraps to the start of the
    /// buffer if there is no match after the cursor.
    SearchSubmit,
    /// Vim `<esc>` while typing a `/` pattern — drop the minibuffer without
    /// changing the cursor or highlights.
    SearchCancel,
    /// Vim `n` — jump to the next match of the most recently submitted
    /// pattern in the same direction. No-op + notify when there is none.
    SearchNext,
    /// Vim `N` — jump to the next match in the reverse of the most recently
    /// submitted direction.
    SearchPrev,

    BufCreate {
        set_active: bool,
        path: Option<Rc<Path>>,
    },
    BufEdit(Rc<Path>),
    BufDelete,
    BufNext,
    BufPrev,
    BufWrite(Option<Rc<Path>>),

    /// Split the focused window; the new pane gets a fresh scratch buffer.
    WindowSplit(SplitDir),
    /// Close the focused window. No-op when only one window remains.
    WindowClose,
    /// Move focus to the next window in tree order, wrapping.
    WindowFocusNext,
    /// Move focus to the nearest window in the given direction.
    WindowFocus(FocusDir),

    /// Bind a key sequence in `mode` to an action. `mode` is a free-form
    /// string so popup-mode bindings (`"popup"`, `"popup.files"`, …) can
    /// live alongside the typed [`EditingMode`] names without expanding
    /// that enum.
    KeymapSet {
        mode: Rc<str>,
        lhs: Vec<KeyEvent>,
        rhs: Rc<Action>,
    },

    KeymapRemove {
        mode: Rc<str>,
        lhs: Vec<KeyEvent>,
    },

    /// Evaluate a pre-parsed lisp form in the editor's runtime. Used to bind
    /// arbitrary lisp expressions to keys: the form lives in the keymap and
    /// is re-evaluated on every keystroke.
    EvalLisp(Rc<Value>),

    /// Request `textDocument/hover` at the focused buffer's cursor and
    /// surface the response as a floating overlay near the cursor.
    LspHover,
    /// Request `textDocument/definition` at the cursor. Single-location
    /// responses jump immediately; multi-location responses open a picker.
    LspGotoDefinition,
    /// Request `textDocument/completion` at the cursor and open a
    /// completion popup. Reused from insert mode.
    LspCompletion,
    /// Request `textDocument/formatting` for the focused buffer and apply
    /// the resulting edits as one tracked changetree node.
    LspFormat,
    /// Request `textDocument/codeAction` at the cursor (or visual range)
    /// and open a picker for the user to choose from.
    LspCodeAction,
    /// Restart a language-server client. `None` restarts the client
    /// attached to the focused buffer; otherwise the named one.
    LspRestart { name: Option<Arc<str>> },
    /// Send `textDocument/didOpen` for the focused buffer. Synthesized by
    /// `BufEdit`/`BufCreate` paths after a fresh LSP attachment is wired.
    LspDidOpenFocused,
    /// Send `textDocument/didClose` for the focused buffer. Synthesized
    /// by buffer-delete / file-path-changed paths.
    LspDidCloseFocused,

    /// Open a hover popup with the given contents anchored at the buffer's
    /// `anchor` absolute position.
    LspShowHover {
        contents: Arc<str>,
        anchor: Position<usize>,
    },
    /// Show a picker over multiple definition locations. Single-location
    /// responses are converted to a direct `BufEdit` + cursor move and
    /// never reach this variant.
    LspShowDefinitionList { locations: Arc<[LocationOwned]> },
    /// Open a completion popup with the given items, anchored at the
    /// position the originating request was issued from.
    LspShowCompletion {
        items: Arc<[CompletionItemOwned]>,
        anchor: Position<usize>,
    },
    /// Open a code-action picker.
    LspShowCodeActions { actions: Arc<[CodeActionOwned]> },
    /// Apply a sorted list of `TextEdit`s to `buf` as a single tracked
    /// changetree node. Used for formatting responses and code-action
    /// `WorkspaceEdit` payloads that target a single buffer.
    LspApplyTextEdits {
        buf: BufferId,
        edits: Arc<[TextEditOwned]>,
        label: Arc<str>,
    },
    /// Apply a multi-document `WorkspaceEdit`. Each entry is one
    /// `LspApplyTextEdits` node under the hood; this variant exists so
    /// the editor can group them under one undo label.
    LspApplyWorkspaceEdit {
        edit: Arc<WorkspaceEditOwned>,
        label: Arc<str>,
    },
    /// Forward a server command back via `workspace/executeCommand`.
    LspExecuteCommand {
        client: LspClientId,
        command: CommandOwned,
    },
}
