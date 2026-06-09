//! Key handling: keymap resolution, count prefix, key-event ring buffer.
//!
//! `handle_key_event` is the entry point the binary's event loop calls; it
//! resolves the keystroke (with chord descent) into an [`rizz_actions::Action`]
//! list and forwards them to [`State::apply`]. `handle_paste` bypasses the
//! keymap so embedded newlines aren't reparsed as `Ctrl+J`.

use std::io;
use std::rc::Rc;
use std::time::Instant;

use crossterm::event::KeyEvent as CTKeyEvent;
use rizz_actions::KeymapRegistry;
use rizz_core::EditingMode;
use rizz_input::KeyEvent;
use tracing::{debug, instrument, trace};

use super::State;

impl State {
    pub fn last_key(&self) -> Option<KeyEvent> {
        self.keyevents.peek_back().map(|(e, _)| e.to_owned())
    }

    pub fn pending_count_or_one(&self) -> u32 {
        self.count_prefix.or_one()
    }

    pub fn keymap_registry(&self) -> &KeymapRegistry {
        &self.keymap
    }

    /// Active keymap modes for the focused input context, most-specific
    /// first: the top panel's named layers, then the buffer's [`EditingMode`].
    pub(super) fn active_modes(&self) -> Vec<Rc<str>> {
        let mut v: Vec<Rc<str>> = self
            .panels
            .top_keymap_layers()
            .iter()
            .rev()
            .cloned()
            .collect();
        let mode = self.bufs[self.focused_buf_id()].mode();
        v.push(mode.as_str().into());
        v
    }

    #[instrument(skip(self), fields(code = ?event.code, mods = ?event.modifiers))]
    pub fn handle_key_event(&mut self, event: CTKeyEvent) -> io::Result<()> {
        let now = Instant::now();
        let timedout = self
            .keyevents
            .peek_back()
            .is_some_and(|(_, earlier)| now.duration_since(*earlier) > self.keycombo_timeout);
        self.keyevents.push_back((event.into(), now));

        let ke: KeyEvent = event.into();
        if self.count_prefix.feed(ke, self.count_eligible()) {
            trace!(?ke, "key consumed by count prefix");
            self.refresh_viewport();
            return self.render();
        }

        let modes = self.active_modes();
        debug!(?ke, ?modes, timedout, "resolving key against keymap");
        if let Some(action) = self.keymap.resolve(&modes, ke, timedout) {
            debug!(
                actions = action.len(),
                "keymap resolved -> applying actions"
            );
            self.apply(&action)?;
            self.count_prefix.clear();
        } else {
            trace!(?ke, "no action resolved (descent or miss)");
        }
        self.refresh_viewport();
        let focused = self.focused_buf_id();
        self.bufs[focused].clamp_cursor();
        self.render()
    }

    /// Insert pasted text as a single edit. The terminal sends OS-level
    /// pastes as one `Event::Paste` (bracketed paste must be enabled on the
    /// terminal); we bypass the keymap entirely so embedded newlines stay as
    /// newlines instead of being parsed as `Ctrl+J` keystrokes.
    #[instrument(skip(self, text), fields(len = text.len()))]
    pub fn handle_paste(&mut self, text: String) -> io::Result<()> {
        if !text.is_empty() {
            self.apply(&[Rc::new(rizz_actions::Action::InsertMany(Rc::from(text)))])?;
        }
        self.refresh_viewport();
        let focused = self.focused_buf_id();
        self.bufs[focused].clamp_cursor();
        self.render()
    }

    pub(super) fn count_eligible(&self) -> bool {
        // A panel on the stack steals input (popup/minibuffer); digits
        // should pass through verbatim.
        if !self.panels.is_empty() {
            return false;
        }
        if !self.keymap.is_idle() {
            return false;
        }
        matches!(
            self.bufs[self.focused_buf_id()].mode(),
            EditingMode::Normal
                | EditingMode::Visual
                | EditingMode::VisualLine
                | EditingMode::VisualBlock
        )
    }
}
