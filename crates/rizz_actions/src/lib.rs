//! Editor actions + keymap.
//!
//! [`Action`] is the closed enum of everything the editor can be asked to do.
//! Every input source — keymap, command line, scripted automation —
//! ultimately produces a list of `Action`s, and there is exactly one
//! interpreter (`State::apply` in `rizz_editor`). Adding new behavior means
//! adding a variant here.
//!
//! [`keymap::KeymapRegistry`] maps layered mode names plus a key sequence to
//! the resulting `Action`. The trie supports partial-sequence carryover and
//! per-node `on_char` wildcards (so insert mode can accept arbitrary typed
//! characters without binding each one explicitly).

pub mod action;
pub mod keymap;

pub use action::Action;
pub use keymap::{KeymapRegistry, default_keymaps};
