mod action;
mod buffer;
mod command;
mod keymap;
mod mode;
mod position;
mod render;
mod render_ratatui;
mod state;
mod terminal;

use std::{io, time::Duration};

use crossterm::event::{self, Event};

use crate::{state::State, terminal::TerminalGuard};

fn main() -> io::Result<()> {
    let _guard = TerminalGuard::new()?;
    let mut state = State::new()?;
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
