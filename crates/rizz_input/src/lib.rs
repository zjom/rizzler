//! Input modelling for the rizz editor.
//!
//! - [`keymap::KeyEvent`] / [`keymap::KeyCode`]: a normalized, crossterm-free
//!   key representation. Round-trips through human-friendly strings like
//!   `"<c-w>q"` via [`keymap::KeyEvent::parse_sequence`].
//! - [`count_prefix::CountPrefix`]: vim-style numeric-prefix accumulator
//!   (`3j`, `12gg`).
//!
//! Higher-level binding tables (the keymap trie / registry that maps key
//! sequences to actions) live in `rizz_actions` so they can reference the
//! `Action` enum directly.

pub mod count_prefix;
pub mod keymap;

pub use count_prefix::CountPrefix;
pub use keymap::{KeyCode, KeyEvent, KeyModifiers};
