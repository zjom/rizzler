use std::{path::PathBuf, str::FromStr};

use crate::action::Action;

/// Maps a typed `:command` string to an [`Action`]. Implement this trait to
/// provide alternate or extended command sets.
pub trait CommandRegistry {
    fn parse(&self, cmd: &str) -> Action;
}

pub struct DefaultCommands;

impl CommandRegistry for DefaultCommands {
    fn parse(&self, input: &str) -> Action {
        let args: Vec<_> = input.trim_ascii().split(' ').collect();
        match args[0] {
            "quit" | "q" => Action::Quit,
            "bufcreate" | "bc" => Action::BufCreate {
                path: None,
                set_active: true,
            },
            "bufdelete" | "bd" => Action::BufDelete,
            "bufprev" | "bp" => Action::BufPrev,
            "bufnext" | "bn" => Action::BufNext,
            "edit" | "e" => {
                if args.len() != 2 {
                    return Action::Noop;
                }
                Action::BufEdit(PathBuf::from_str(args[1]).unwrap().into())
            }
            "write" | "w" => {
                if args.len() == 2 {
                    return Action::BufWrite(Some(PathBuf::from_str(args[1]).unwrap().into()));
                }
                Action::BufWrite(None)
            }
            _ => Action::Noop,
        }
    }
}
