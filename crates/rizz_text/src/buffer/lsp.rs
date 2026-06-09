//! LSP attachment hook for `Buffer`.
//!
//! The concrete implementation lives in `rizz_lsp` (which depends on tokio).
//! To keep `rizz_text` free of tokio/serde the buffer holds a type-erased
//! `Box<dyn LspBufferHandle>` and only the `rizz_core::LspDiagnostic` POD
//! crosses the boundary.

use ropey::Rope;

use rizz_core::LspDiagnostic;

/// Behaviours every LSP client attachment must provide so the buffer can
/// drive the document lifecycle. Methods take `&mut self` because the
/// attachment buffers pending changes and a version counter internally.
pub trait LspBufferHandle: std::fmt::Debug + Send {
    /// Send `textDocument/didOpen` to the server. Called once when the
    /// attachment is first installed on a buffer with a known path.
    fn did_open(&mut self, rope: &Rope);

    /// Record a single rope splice as a pending didChange edit. The
    /// attachment converts to LSP coordinates internally and ships the
    /// batched changes to the runtime; the actual `textDocument/didChange`
    /// notification is debounced server-side.
    fn record_edit(&mut self, at_char: usize, removed: &str, inserted: &str, rope: &Rope);

    /// Send `textDocument/didClose`. Called when the buffer is dropped or
    /// the file path changes underneath it.
    fn did_close(&mut self);

    /// Latest diagnostics for this buffer, in arbitrary order. Updated by
    /// the editor when a `publishDiagnostics` notification arrives.
    fn diagnostics(&self) -> &[LspDiagnostic];

    /// Replace the buffer's stored diagnostics wholesale. Called from the
    /// editor's drain loop on every `publishDiagnostics` notification —
    /// LSP semantics say the latest batch is authoritative.
    fn replace_diagnostics(&mut self, items: Vec<LspDiagnostic>);
}
