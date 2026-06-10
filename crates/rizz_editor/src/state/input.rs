//! Key handling: keymap resolution, count prefix, key-event ring buffer.
//!
//! `handle_key_event` is the entry point the binary's event loop calls; it
//! resolves the keystroke (with chord descent) into an [`rizz_actions::Action`]
//! list and forwards them to [`State::apply`]. `handle_paste` bypasses the
//! keymap so embedded newlines aren't reparsed as `Ctrl+J`.

use std::io;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crossterm::event::KeyEvent as CTKeyEvent;
use rizz_actions::KeymapRegistry;
use rizz_core::EditingMode;
use rizz_input::{CountPrefix, KeyEvent};
use rizz_ringbuffer::RingBuffer;
use tracing::{debug, instrument, trace};

use super::State;

/// Keymap dispatch + the inputs feeding it. Owns the keymap registry, the
/// rolling key-event buffer (for `:keys` / debugging), the count prefix that
/// scales motions and operators, and the chord-timeout knob.
pub(super) struct Input {
    pub keymap: KeymapRegistry,
    pub keyevents: RingBuffer<(KeyEvent, Instant), 100>,
    pub keycombo_timeout: Duration,
    pub count_prefix: CountPrefix,
}

impl Input {
    pub(super) fn new(keycombo_timeout: Duration) -> Self {
        Self {
            keymap: KeymapRegistry::new(),
            keyevents: RingBuffer::new(),
            keycombo_timeout,
            count_prefix: CountPrefix::new(),
        }
    }
}

impl State {
    pub fn last_key(&self) -> Option<KeyEvent> {
        self.input.keyevents.peek_back().map(|(e, _)| e.to_owned())
    }

    pub fn pending_count_or_one(&self) -> u32 {
        self.input.count_prefix.or_one()
    }

    pub fn keymap_registry(&self) -> &KeymapRegistry {
        &self.input.keymap
    }

    /// Active keymap modes for the focused input context, most-specific
    /// first: the top panel's named layers, then the buffer's [`EditingMode`].
    pub(super) fn active_modes(&self) -> Vec<Rc<str>> {
        let mut v: Vec<Rc<str>> = self
            .surface
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
            .input
            .keyevents
            .peek_back()
            .is_some_and(|(_, earlier)| now.duration_since(*earlier) > self.input.keycombo_timeout);
        self.input.keyevents.push_back((event.into(), now));

        let ke: KeyEvent = event.into();
        if self.input.count_prefix.feed(ke, self.count_eligible()) {
            trace!(?ke, "key consumed by count prefix");
            self.refresh_viewport();
            return self.render();
        }

        let modes = self.active_modes();
        debug!(?ke, ?modes, timedout, "resolving key against keymap");
        if let Some(action) = self.input.keymap.resolve(&modes, ke, timedout) {
            debug!(
                actions = action.len(),
                "keymap resolved -> applying actions"
            );
            self.apply(&action);
            self.input.count_prefix.clear();
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
            self.apply(&[Rc::new(rizz_actions::Action::InsertMany(Rc::from(text)))]);
        }
        self.refresh_viewport();
        let focused = self.focused_buf_id();
        self.bufs[focused].clamp_cursor();
        self.render()
    }

    pub(super) fn count_eligible(&self) -> bool {
        // A panel on the stack steals input (popup/minibuffer); digits
        // should pass through verbatim.
        if !self.surface.panels.is_empty() {
            return false;
        }
        if !self.input.keymap.is_idle() {
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
