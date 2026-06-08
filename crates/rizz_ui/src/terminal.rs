//! RAII terminal lifecycle: alt screen, raw mode, mouse + focus capture, and
//! kitty keyboard protocol when supported. [`TerminalGuard::new`] puts the
//! terminal into editor mode; the `Drop` impl (plus a panic hook installed
//! by [`install_panic_hook`]) restore it on the way out.

use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
};

use crossterm::{
    event::{
        DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
        supports_keyboard_enhancement,
    },
};

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
        DisableBracketedPaste,
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
            EnableBracketedPaste,
        )?;
        enable_raw_mode()?;

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
