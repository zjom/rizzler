use crate::mode::EditingMode;

/// Every input source (keymap, command line, scripted automation) ultimately
/// produces an [`Action`]. [`crate::state::State::apply`] is the single point
/// that interprets them, so adding a new behavior means adding a variant here
/// and a match arm in `apply`.
#[derive(Debug, Clone, Copy)]
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

    BufCreate,
    BufDelete,
    BufNext,
    BufPrev,
}
