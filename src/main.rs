mod action;
mod buffer;
mod command;
mod keymap;
mod mode;
mod position;
mod render;
mod state;
mod terminal;

use std::{io, time::Duration};

use crossterm::{
    event::{self, Event},
    terminal::size,
};

use crate::{state::State, terminal::TerminalGuard};

fn main() -> io::Result<()> {
    let _guard = TerminalGuard::new()?;
    let (cols, rows) = size()?;
    let mut state = State::new(io::stdout(), cols, rows)?;

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
