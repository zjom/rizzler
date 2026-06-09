use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error(
        "grammar `{name}` is not in the manifest — add an entry to grammars.toml or pass {{\"path\": …}} / {{\"repo\": …}}"
    )]
    UnknownGrammar { name: String },

    #[error("manifest at {path} could not be parsed: {source}")]
    Manifest {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("grammar source dir does not exist: {path}")]
    MissingSource { path: PathBuf },

    #[error("`{tool}` is not on PATH (or failed to launch): {source}")]
    ToolNotFound {
        tool: &'static str,
        #[source]
        source: std::io::Error,
    },

    #[error("git failed during {step} (exit {status:?}): {stderr}")]
    Git {
        step: &'static str,
        status: Option<i32>,
        stderr: String,
    },

    #[error("`tree-sitter build` failed (exit {status:?}) in {dir}: {stderr}")]
    Build {
        dir: PathBuf,
        status: Option<i32>,
        stderr: String,
    },

    #[error(
        "highlights query not found at {path} — pass {{\"queries\": …}} to point at the right file"
    )]
    MissingHighlights { path: PathBuf },

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}
