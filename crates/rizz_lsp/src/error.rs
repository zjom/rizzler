use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum LspError {
    #[error("failed to spawn language server `{name}` ({command:?}): {source}")]
    Spawn {
        name: String,
        command: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("language server `{name}` did not respond to `initialize` within {timeout_ms}ms")]
    InitializeTimeout { name: String, timeout_ms: u64 },

    #[error("language server `{name}` returned an error during initialize: {message}")]
    InitializeFailed { name: String, message: String },

    #[error("language server `{name}` has crashed")]
    Crashed { name: String },

    #[error("language server `{name}` shut down before request completed")]
    Cancelled { name: String },

    #[error("malformed lsp frame: {reason}")]
    Frame { reason: String },

    #[error("io error: {source}")]
    Io {
        #[from]
        source: std::io::Error,
    },

    #[error("json error: {source}")]
    Json {
        #[from]
        source: serde_json::Error,
    },
}
