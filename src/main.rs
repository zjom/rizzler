mod action;
mod buffer;
mod buffer_io;
mod buffer_list;
mod count_prefix;
mod journal;
mod keymap;
mod lisp;
mod mode;
mod motions;
mod position;
mod selection;
mod state;
mod ui;

use std::{io, path::PathBuf, time::Duration};

use crossterm::event::{self, Event};

use crate::{
    state::{Config, State},
    ui::terminal::TerminalGuard,
};

fn main() -> io::Result<()> {
    ui::terminal::install_panic_hook();
    let _guard = TerminalGuard::new()?;
    let path = std::env::args_os().nth(1).map(PathBuf::from);

    let mut state = State::with_config(Config::with_path(path)?)?;
    state.render()?; // initial render
    loop {
        if state.quit_requested() {
            break;
        }

        if event::poll(Duration::from_millis(500))?
            && let Event::Key(key_event) = event::read()?
        {
            state.handle_key_event(key_event)?;
        }
    }

    Ok(())
}
