//! LSP diagnostic POD shared across crates.
//!
//! Lives in `rizz_core` so `rizz_text` can store diagnostics on a buffer
//! without depending on `lsp-types`, `rizz_actions` can synthesize actions
//! that carry them, and `rizz_ui` can render them — all without leaking
//! tokio/serde into the rope core. The `rizz_lsp` crate converts
//! `lsp_types::Diagnostic` into this shape at the boundary.
//!
//! Ranges are stored as `(line, col)` pairs in UTF-8 byte columns — the
//! editor's native coordinate space. The LSP client does any UTF-16
//! adjustment before constructing one of these.

use crate::Position;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

impl Severity {
    /// Face name used to look up the visual style in the theme registry.
    /// Mirrors the `syntax.*` family populated by the tree-sitter renderer.
    pub fn face(self) -> &'static str {
        match self {
            Severity::Error => "diagnostic.error",
            Severity::Warning => "diagnostic.warning",
            Severity::Info => "diagnostic.info",
            Severity::Hint => "diagnostic.hint",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LspDiagnostic {
    pub start: Position<usize>,
    pub end: Position<usize>,
    pub severity: Severity,
    pub message: std::sync::Arc<str>,
    pub source: Option<std::sync::Arc<str>>,
    pub code: Option<std::sync::Arc<str>>,
}
