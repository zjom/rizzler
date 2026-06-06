mod action;
mod buffer;
mod buffer_io;
mod buffer_list;
mod components;
mod count_prefix;
mod journal;
mod keymap;
mod lisp;
mod mode;
mod motions;
mod popup;
mod position;
mod precompute;
mod props;
mod regions;
mod render;
mod render_ratatui;
mod scroll;
mod selection;
mod state;
mod styling;
mod terminal;
mod window;
mod wrap;

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
