#![allow(dead_code)]
use std::path::Path;
use std::rc::Rc;

use crate::keymap::KeyEvent;
use crate::mode::EditingMode;
use crate::position::Position;
use crate::window::{FocusDir, SplitDir};

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

    CommandSubmit,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Copy)]
pub enum MoveKind {
    LineStart,
    LineEnd,
    FileStart,
    FileEnd,
    WordStart,
    WordEnd,
    Relative(Position<i16>),   // up, down, left, right of cursor
    Absolute(Position<usize>), // position in file
    LineNum(usize),
    HalfPageDown,
    HalfPageUp,
    /// Vim's `zz` — re-center the viewport on the cursor without moving it.
    Center,
}
