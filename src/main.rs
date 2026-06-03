mod action;
mod buffer;
mod components;
mod keymap;
mod lisp;
mod mode;
mod position;
mod props;
mod render;
mod render_ratatui;
mod slots;
mod state;
mod styling;
mod terminal;
mod window;

use std::{io, time::Duration};

use crossterm::event::{self, Event};

use crate::{state::State, terminal::TerminalGuard};

fn main() -> io::Result<()> {
    terminal::install_panic_hook();
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
