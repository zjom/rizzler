use std::io;

use crossterm::{
    event::{
        DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        self, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
        supports_keyboard_enhancement,
    },
};

/// RAII guard that puts the terminal into raw / alt-screen mode on
/// construction and restores it on drop. Using this instead of paired
/// setup/teardown calls means a panic mid-session still leaves the terminal
/// usable.
pub struct TerminalGuard {
    /// Tracks whether we pushed kitty keyboard flags so Drop only pops if we
    /// actually pushed. Terminals that don't support the protocol leave the
    /// stack untouched.
    kitty_pushed: bool,
}

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

        // Kitty keyboard protocol disambiguates control characters that
        // otherwise alias to legacy keys (Ctrl-H vs Backspace, Ctrl-J vs
        // Enter, Ctrl-I vs Tab, Ctrl-M vs Enter). Only push on terminals
        // that advertise support — otherwise the escape is ignored but the
        // pop on Drop would also do nothing, so it's a wash either way.
        let kitty_pushed = supports_keyboard_enhancement().unwrap_or(false);
        if kitty_pushed {
            execute!(
                io::stdout(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,),
            )?;
        }

        Ok(Self { kitty_pushed })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.kitty_pushed {
            let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        }
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableFocusChange,
            DisableMouseCapture,
        );
    }
}
