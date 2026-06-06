//! The closed enum of every behaviour the editor can be asked to perform.
//!
//! Every input source ultimately produces an [`Action`], and
//! `rizz_editor::State::apply` is the single point that interprets them — so
//! adding new behaviour means adding a variant here and a match arm in
//! `apply`. Resist the urge to grow new entry points; the single-funnel
//! invariant is load-bearing for undo, scripting, and tests.

use std::path::Path;
use std::rc::Rc;

use rizz::runtime::Value;
use rizz_core::{EditingMode, FocusDir, Position, SplitDir};
use rizz_input::KeyEvent;
use rizz_text::MoveKind;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Action {
    Noop,
    Quit,
    SetMode(EditingMode),

    InsertChar(char),
    InsertNewline,
    DeleteChar,
    DeleteCharAt(Position<usize>),
    /// Reverse the focused buffer's most recent tracked edit.
    Undo,
    /// Re-apply the most recently undone edit on the focused buffer.
    Redo,
    /// Move the cursor by `kind`, repeated `count` times (0/1 == once).
    MoveCursor {
        kind: MoveKind,
        count: u32,
    },

    CommandCancel,

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
}
