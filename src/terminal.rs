use std::io;

use crossterm::{
    event::{DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture},
    execute,
    terminal::{
        self, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    },
};

/// RAII guard that puts the terminal into raw / alt-screen mode on
/// construction and restores it on drop. Using this instead of paired
/// setup/teardown calls means a panic mid-session still leaves the terminal
/// usable.
pub struct TerminalGuard;

impl TerminalGuard {
    pub fn new() -> io::Result<Self> {
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            terminal::Clear(terminal::ClearType::All),
            EnableFocusChange,
            EnableMouseCapture,
        )?;
        enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableFocusChange,
            DisableMouseCapture,
        );
    }
}
