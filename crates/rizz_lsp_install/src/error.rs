//! Errors surfaced when resolving or installing an LSP server binary.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error(
        "lsp server `{name}` is not in the manifest — add an entry to lsp.toml or pass {{\"command\": …}} via `(lsp-install)`"
    )]
    UnknownServer { name: String },

    #[error("manifest at {path} could not be parsed: {source}")]
    Manifest {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error(
        "lsp server `{name}`: binary `{command}` not found on $PATH and no `install` recipe is set"
    )]
    NotOnPathAndNoRecipe { name: String, command: String },

    #[error("install recipe for `{name}` failed (exit {status:?}): {stderr}")]
    Recipe {
        name: String,
        status: Option<i32>,
        stderr: String,
    },

    #[error("install recipe for `{name}` ran but did not produce {expected:?}")]
    RecipeMissingOutput { name: String, expected: PathBuf },

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}
