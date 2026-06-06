//! Keymap registry: maps layered mode names + a key sequence to an
//! [`crate::Action`]. See [`registry::KeymapRegistry`].

mod default;
mod registry;
mod trie;

pub use default::default_keymaps;
pub use registry::{KeymapRegistry, KeymapRegistryIter};
pub use trie::{Trie, TrieIter, WalkOutcome, walk};
