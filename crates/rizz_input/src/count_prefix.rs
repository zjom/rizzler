//! Vim-style numeric prefix accumulator. In Normal / Visual modes, digits
//! typed while no keymap sequence is in flight feed [`CountPrefix`] instead
//! of resolving against the keymap — `3j`, `12gg`, etc. `0` only counts once
//! a digit has already been seen (it stays bound to `line-start` when the
//! count is empty).

use crate::keymap::{KeyCode, KeyEvent};

#[derive(Default, Debug, Clone, Copy)]
pub struct CountPrefix {
    value: Option<u32>,
}

impl CountPrefix {
    pub fn new() -> Self {
        Self::default()
    }

    /// Count to attach to the next motion. Returns 1 when nothing has been
    /// typed so callers can blindly multiply.
    pub fn or_one(&self) -> u32 {
        self.value.unwrap_or(1)
    }

    /// Drain the accumulator. Call after a key resolves to an action.
    pub fn clear(&mut self) {
        self.value = None;
    }

    /// Try to absorb `ke` as a digit. `eligible` is the caller's gate (mode
    /// is Normal/Visual, keymap idle, no popup or minibuffer focus); when
    /// `false` the call is a no-op. Returns `true` iff the digit was eaten
    /// and should *not* fall through to keymap resolution.
    pub fn feed(&mut self, ke: KeyEvent, eligible: bool) -> bool {
        if !eligible {
            return false;
        }
        let KeyCode::Char(c) = ke.code else {
            return false;
        };
        if !c.is_ascii_digit() {
            return false;
        }
        let d = (c as u8 - b'0') as u32;
        if d == 0 && self.value.is_none() {
            return false;
        }
        let cur = self.value.unwrap_or(0);
        self.value = Some(cur.saturating_mul(10).saturating_add(d));
        true
    }
}
