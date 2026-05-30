use std::path::PathBuf;
use std::str::FromStr;

use crate::keymap::KeyEvent;
use crate::mode::EditingMode;

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
    MoveCursor(i16, i16),

    CommandPush(char),
    CommandPop,
    CommandSubmit,
    CommandCancel,

    BufCreate {
        set_active: bool,
        path: Option<PathBuf>,
    },
    BufEdit(PathBuf),
    BufDelete,
    BufNext,
    BufPrev,
    BufWrite,

    KeymapSet {
        mode: EditingMode,
        lhs: Vec<KeyEvent>,
        rhs: Box<Action>,
    },

    KeymapRemove {
        mode: EditingMode,
        lhs: Vec<KeyEvent>,
    },
}
