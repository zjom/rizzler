use std::path::PathBuf;
use tree_sitter::{LanguageError, QueryError};

#[derive(Debug, thiserror::Error)]
pub enum TsError {
    #[error("invalid grammar ABI: {0}")]
    Abi(#[from] LanguageError),
    #[error("highlights query did not compile: {0}")]
    Query(#[from] QueryError),
    #[error("failed to load grammar library {path}: {source}")]
    LoadLibrary {
        path: PathBuf,
        #[source]
        source: libloading::Error,
    },
    #[error("grammar library missing symbol `tree_sitter_{name}`: {source}")]
    MissingSymbol {
        name: String,
        #[source]
        source: libloading::Error,
    },
}
