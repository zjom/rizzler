use std::{io, path::PathBuf, time::Duration};

use crossterm::event::{self, Event};

use rizz_editor::{Config, State};
use rizz_ui::{TerminalGuard, install_panic_hook};

fn main() -> io::Result<()> {
    install_panic_hook();
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
