use crate::action::Action;

/// Maps a typed `:command` string to an [`Action`]. Implement this trait to
/// provide alternate or extended command sets.
pub trait CommandRegistry {
    fn parse(&self, cmd: &str) -> Action;
}

pub struct DefaultCommands;

impl CommandRegistry for DefaultCommands {
    fn parse(&self, cmd: &str) -> Action {
        match cmd {
            "quit" | "q" => Action::Quit,
            "bufcreate" | "bc" => Action::BufCreate,
            "bufdelete" | "bd" => Action::BufDelete,
            "bufprev" | "bp" => Action::BufPrev,
            "bufnext" | "bn" => Action::BufNext,
            _ => Action::Noop,
        }
    }
}
