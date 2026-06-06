//! Normalized key events. Higher-level keymap tries / registries live in
//! `rizz_actions::keymap` because they need to reference `Action`.

mod event;

pub use event::{KeyCode, KeyEvent, KeyModifiers};
