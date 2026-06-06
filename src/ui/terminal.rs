use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
};

use crossterm::{
    event::{
        DisableFocusChange, DisableMouseCapture, EnableFocusChange, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
        supports_keyboard_enhancement,
    },
};

/// Tracks whether kitty flags were pushed so the panic hook can mirror Drop's
/// conditional pop without needing a handle to the guard.
static KITTY_PUSHED: AtomicBool = AtomicBool::new(false);

fn restore_terminal() {
    if KITTY_PUSHED.swap(false, Ordering::SeqCst) {
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

/// Installs a panic hook that leaves the alt screen before the default hook
/// prints the panic message — otherwise the message renders into the alt
/// screen and is wiped when [`TerminalGuard`]'s Drop fires during unwind.
pub fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        prev(info);
    }));
}

/// RAII guard that puts the terminal into raw / alt-screen mode on
/// construction and restores it on drop. Using this instead of paired
/// setup/teardown calls means a panic mid-session still leaves the terminal
/// usable.
pub struct TerminalGuard {
    _private: (),
}

impl TerminalGuard {
    pub fn new() -> io::Result<Self> {
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableFocusChange,
            EnableMouseCapture,
        )?;
        enable_raw_mode()?;

        // Kitty keyboard protocol disambiguates control characters that
        // otherwise alias to legacy keys (Ctrl-H vs Backspace, Ctrl-J vs
        // Enter, Ctrl-I vs Tab, Ctrl-M vs Enter). Only push on terminals
        // that advertise support — otherwise the escape is ignored but the
        // pop on Drop would also do nothing, so it's a wash either way.
        if supports_keyboard_enhancement().unwrap_or(false) {
            execute!(
                io::stdout(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,),
            )?;
            KITTY_PUSHED.store(true, Ordering::SeqCst);
        }

        Ok(Self { _private: () })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}
