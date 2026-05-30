#![allow(dead_code)]
use std::path::Path;
use std::rc::Rc;

use crate::keymap::KeyEvent;
use crate::mode::EditingMode;
use crate::position::Position;

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

    CommandPush(char),
    CommandPop,
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
