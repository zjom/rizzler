#![allow(dead_code)]
use crate::buffer::MoveKind;
use std::path::Path;
use std::rc::Rc;

use crate::keymap::KeyEvent;
use crate::mode::EditingMode;
use crate::window::{FocusDir, SplitDir};
use rizz::runtime::Value;

/// Every input source (keymap, command line, scripted automation) ultimately
/// produces an [`Action`]. [`crate::state::State::apply`] is the single point
/// that interprets them, so adding a new behavior means adding a variant here
/// and a match arm in `apply`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Action {
    Noop,
    Quit,
    SetMode(EditingMode),

    InsertChar(char),
    InsertNewline,
    DeleteChar,
    MoveCursor(MoveKind),

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

    KeymapSet {
        mode: EditingMode,
        lhs: Vec<KeyEvent>,
        rhs: Rc<Action>,
    },

    KeymapRemove {
        mode: EditingMode,
        lhs: Vec<KeyEvent>,
    },

    /// Evaluate a pre-parsed lisp form in the editor's runtime. Used to bind
    /// arbitrary lisp expressions to keys: the form lives in the keymap and
    /// is re-evaluated on every keystroke.
    EvalLisp(Rc<Value>),
}
