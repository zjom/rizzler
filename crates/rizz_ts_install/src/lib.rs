//! Declarative installer for tree-sitter grammars.
//!
//! `rizz_ts` is intentionally small — it loads a shared library and hands back
//! a highlighter. This crate sits above it and answers the question *"where
//! does the shared library come from?"*: a curated TOML manifest names known
//! grammars, this crate clones their sources via `git`, builds them via the
//! user's `tree-sitter` CLI, and caches the resulting `parser.{so|dylib|dll}`
//! plus `highlights.scm` under `$XDG_DATA_HOME/rizz/grammars/<name>/`.
//!
//! Two entry points for the embedding editor:
//!
//! * [`install`] — fetch + build + cache + return the resolved paths. The
//!   editor follows this with a call to `TsRegistry::register`. Safe to call
//!   repeatedly: a matching cache stamp short-circuits the network.
//! * [`try_load_cached`] — pure cache lookup, no shell-outs. Used on buffer
//!   open to register a grammar that was installed in a previous session.

mod cache;
mod error;
mod install;
mod manifest;

pub use cache::{CachedGrammar, cache_root, library_filename};
pub use error::InstallError;
pub use install::{InstallOpts, InstalledGrammar, install, try_load_cached};
pub use manifest::{GrammarSpec, Manifest};

/// Read the highlights query off disk for an installed grammar. Thin wrapper
/// over `std::fs::read_to_string` so callers don't have to know the file is
/// just text.
pub fn read_highlights(g: &InstalledGrammar) -> std::io::Result<String> {
    std::fs::read_to_string(&g.highlights)
}
