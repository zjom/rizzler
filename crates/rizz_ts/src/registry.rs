use crate::{Highlighter, TsError};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use libloading::{Library, Symbol};
use tree_sitter::{Language as TsLanguage, Parser, Query};
use tree_sitter_language::LanguageFn;

/// A loaded grammar living in a shared library. The `Library` must outlive
/// every [`Highlighter`] that references this grammar — dropping it would
/// dangle the `TsLanguage` pointer and every `Query` compiled against it.
/// Held in an `Rc` so the registry and any highlighter built from it share
/// ownership.
pub struct TsGrammar {
    pub name: Rc<str>,
    pub(crate) language: TsLanguage,
    /// Compiled once at registration; reused (via `Rc`) by every highlighter
    /// the registry hands out for this grammar.
    pub(crate) query: Rc<Query>,
    pub(crate) capture_names: Rc<[Rc<str>]>,
    /// Kept alive so `language` and `query` stay valid. Field name starts
    /// with `_` because nothing reads it directly — its only job is `Drop`
    /// ordering.
    _library: Library,
}

impl std::fmt::Debug for TsGrammar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grammar").field("name", &self.name).finish()
    }
}

/// Runtime registry of dynamically-loaded tree-sitter grammars, indexed by
/// file extension (lowercase, no leading dot). The editor's `State` owns one;
/// every buffer load consults it to install a highlighter for known
/// extensions.
#[derive(Default)]
pub struct TsRegistry {
    by_ext: HashMap<Rc<str>, Rc<TsGrammar>>,
}

impl TsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load a grammar from `library_path`, compile `highlights_query` against
    /// it, and index it by every extension in `extensions` (leading `.`
    /// optional, normalised to lowercase). Errors leave the registry
    /// untouched.
    ///
    /// `name` is the language identifier embedded in the C symbol the library
    /// exports — by Neovim convention, a library built from
    /// `tree-sitter-python`'s `parser.c` exports `tree_sitter_python`, so
    /// `name` would be `"python"`.
    pub fn register(
        &mut self,
        name: &str,
        extensions: &[String],
        library_path: &Path,
        highlights_query: &str,
    ) -> Result<(), TsError> {
        // SAFETY: Loading a shared library runs its initialisers; we trust
        // the user-provided path. ABI mismatch is caught downstream by
        // `parser.set_language` returning `LanguageError`.
        let library = unsafe {
            Library::new(library_path).map_err(|source| TsError::LoadLibrary {
                path: library_path.to_path_buf(),
                source,
            })?
        };
        // Resolve the `tree_sitter_<name>` factory, then copy the raw fn
        // pointer out. The `Symbol` borrows `library`; the bare fn pointer
        // stays valid for as long as `library` lives.
        let symbol = format!("tree_sitter_{name}");
        let raw: unsafe extern "C" fn() -> *const () = unsafe {
            let sym: Symbol<unsafe extern "C" fn() -> *const ()> =
                library
                    .get(symbol.as_bytes())
                    .map_err(|source| TsError::MissingSymbol {
                        name: name.to_string(),
                        source,
                    })?;
            *sym
        };
        // SAFETY: The contract on `tree_sitter_<name>` is that it returns a
        // `&'static TSLanguage`. `library` is held in the `Grammar` we
        // build below, which keeps that storage live.
        let language: TsLanguage = unsafe { LanguageFn::from_raw(raw) }.into();
        // Pre-flight: build a parser + query to catch ABI mismatches and
        // bad queries before any state mutation.
        let mut parser = Parser::new();
        parser.set_language(&language)?;
        let query = Query::new(&language, highlights_query)?;
        let capture_names: Rc<[Rc<str>]> = query
            .capture_names()
            .iter()
            .map(|n| Rc::<str>::from(*n))
            .collect::<Vec<_>>()
            .into();
        let grammar = Rc::new(TsGrammar {
            name: name.into(),
            language,
            query: Rc::new(query),
            capture_names,
            _library: library,
        });
        for ext in extensions {
            let normalized = ext.trim_start_matches('.').to_ascii_lowercase();
            self.by_ext
                .insert(Rc::from(normalized.as_str()), grammar.clone());
        }
        Ok(())
    }

    /// Build a highlighter for `path`, matched on the lowercased file
    /// extension. Returns `None` when no registered grammar matches.
    pub fn highlighter_for_path(&self, path: &Path) -> Option<Highlighter> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())?
            .to_ascii_lowercase();
        let grammar = self.by_ext.get(ext.as_str())?.clone();
        Some(Highlighter::new(grammar))
    }

    /// Number of registered extension → grammar mappings. Multiple extensions
    /// pointing at the same `Grammar` count separately.
    pub fn len(&self) -> usize {
        self.by_ext.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_ext.is_empty()
    }
}

impl std::fmt::Debug for TsRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TsRegistry")
            .field("registered_extensions", &self.by_ext.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_returns_none() {
        let reg = TsRegistry::new();
        assert!(reg.highlighter_for_path(Path::new("foo.rs")).is_none());
    }

    #[test]
    fn registry_returns_none_for_path_without_extension() {
        let reg = TsRegistry::new();
        assert!(reg.highlighter_for_path(Path::new("Makefile")).is_none());
    }

    #[test]
    fn register_rejects_missing_library() {
        let mut reg = TsRegistry::new();
        let err = reg.register(
            "fake",
            &["fake".to_string()],
            Path::new("/path/does/not/exist.dylib"),
            "; empty query",
        );
        assert!(matches!(err, Err(TsError::LoadLibrary { .. })));
        assert!(reg.is_empty(), "registry must stay empty after failure");
    }
}
