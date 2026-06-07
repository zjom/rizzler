//! Tree-sitter integration for the rizz editor.
//!
//! All grammars are loaded at runtime from shared libraries (`.so` /
//! `.dylib` / `.dll`) that export a `tree_sitter_<name>()` C symbol — the
//! same ABI Neovim uses for `parser/*.so`. The editor ships with none; user
//! config registers them via the [`TsRegistry`].
//!
//! Flow per buffer:
//! 1. `TsRegistry::register(...)` loads a library, resolves the factory,
//!    compiles the highlights query, and indexes the resulting [`TsGrammar`]
//!    by file extension.
//! 2. `TsRegistry::highlighter_for_path(...)` hands back a [`Highlighter`]
//!    sharing the cached `Query` and language pointer.
//! 3. Feed text in via [`Highlighter::set_source`], call
//!    [`Highlighter::ensure_parsed`] to refresh the tree, then
//!    [`Highlighter::query`] to iterate styled captures clipped to a byte
//!    range — typically the visible viewport.
//!
//! Capture names follow the conventional `nvim-treesitter` shorthand
//! (`keyword`, `string`, `function`, …); the renderer maps them to face
//! names by prepending `"syntax."` (see `rizz_ui::precompute`).

mod error;
mod highlighter;
mod registry;

pub use error::TsError;
pub use highlighter::{HighlightSpan, Highlighter};
pub use registry::{TsGrammar, TsRegistry};
