use crossterm::event::{KeyCode, KeyEvent};

use crate::{action::Action, mode::EditingMode};

/// Resolves a key event in a given mode to an [`Action`]. Implement this
/// trait to provide alternate key bindings (e.g. an emacs-style keymap).
pub trait Keymap {
    fn resolve(&self, mode: EditingMode, key: KeyEvent) -> Action;
}

pub struct DefaultKeymap;

impl Keymap for DefaultKeymap {
    fn resolve(&self, mode: EditingMode, key: KeyEvent) -> Action {
        match mode {
            EditingMode::Command => match key.code {
                KeyCode::Enter => Action::CommandSubmit,
                KeyCode::Char(c) => Action::CommandPush(c),
                KeyCode::Backspace => Action::CommandPop,
                KeyCode::Esc => Action::CommandCancel,
                _ => Action::Noop,
            },
            EditingMode::Insert => match key.code {
                KeyCode::Enter => Action::InsertNewline,
                KeyCode::Char(c) => Action::InsertChar(c),
                KeyCode::Backspace => Action::DeleteChar,
                KeyCode::Esc => Action::SetMode(EditingMode::Normal),
                _ => Action::Noop,
            },
            EditingMode::Normal => match key.code {
                KeyCode::Char(':') => Action::SetMode(EditingMode::Command),
                KeyCode::Char('i') => Action::SetMode(EditingMode::Insert),
                KeyCode::Char('j') | KeyCode::Down => Action::MoveCursor(0, 1),
                KeyCode::Char('k') | KeyCode::Up => Action::MoveCursor(0, -1),
                KeyCode::Char('h') | KeyCode::Left => Action::MoveCursor(-1, 0),
                KeyCode::Char('l') | KeyCode::Right => Action::MoveCursor(1, 0),
                _ => Action::Noop,
            },
            EditingMode::Visual => Action::Noop,
        }
    }
}
