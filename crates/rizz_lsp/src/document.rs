//! Concrete `LspBufferHandle` implementation.
//!
//! Owns the per-document state the editor side needs to drive the LSP
//! lifecycle: URI, language id, version counter, and pending diagnostics.
//! Edits are converted to LSP coordinates here and dispatched to the tokio
//! side, where the per-client task batches them on a debounce timer.

use rizz_actions::LspClientId;
use rizz_core::LspDiagnostic;
use rizz_text::LspBufferHandle;
use ropey::Rope;
use tracing::debug;

use crate::event::{ChangeEvent, RuntimeCmd};
use crate::position::{Encoding, advance_position, byte_to_lsp};
use crate::runtime::runtime;

pub struct LspBufferAttachment {
    client: LspClientId,
    uri: String,
    language_id: String,
    encoding: Encoding,
    version: i32,
    diagnostics: Vec<LspDiagnostic>,
    diagnostics_gen: u64,
}

impl LspBufferAttachment {
    pub fn new(client: LspClientId, uri: String, language_id: String, encoding: Encoding) -> Self {
        Self {
            client,
            uri,
            language_id,
            encoding,
            version: 0,
            diagnostics: Vec::new(),
            diagnostics_gen: 0,
        }
    }

    pub fn client(&self) -> LspClientId {
        self.client
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }

    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    pub fn version(&self) -> i32 {
        self.version
    }
}

impl std::fmt::Debug for LspBufferAttachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspBufferAttachment")
            .field("client", &self.client)
            .field("uri", &self.uri)
            .field("encoding", &self.encoding)
            .field("version", &self.version)
            .finish()
    }
}

impl LspBufferHandle for LspBufferAttachment {
    fn did_open(&mut self, rope: &Rope) {
        self.version = 1;
        runtime().send_cmd(RuntimeCmd::DidOpen {
            client: self.client,
            uri: self.uri.clone(),
            language_id: self.language_id.clone(),
            version: self.version,
            text: rope.to_string(),
        });
        debug!(uri = %self.uri, "lsp didOpen");
    }

    fn record_edit(&mut self, at_char: usize, removed: &str, inserted: &str, rope: &Rope) {
        let row = rope.char_to_line(at_char);
        let line_start_byte = rope.line_to_byte(row);
        let start_byte_in_rope = rope.char_to_byte(at_char);
        let start_byte_col = start_byte_in_rope - line_start_byte;
        let start = byte_to_lsp(rope, row, start_byte_col, self.encoding);
        let end = advance_position(start, removed, self.encoding);
        let range = lsp_types::Range { start, end };
        self.version += 1;
        runtime().send_cmd(RuntimeCmd::DidChange {
            client: self.client,
            uri: self.uri.clone(),
            version: self.version,
            changes: vec![ChangeEvent {
                range: Some(range),
                text: inserted.to_string(),
            }],
        });
    }

    fn did_close(&mut self) {
        runtime().send_cmd(RuntimeCmd::DidClose {
            client: self.client,
            uri: self.uri.clone(),
        });
        debug!(uri = %self.uri, "lsp didClose");
    }

    fn diagnostics(&self) -> &[LspDiagnostic] {
        &self.diagnostics
    }

    fn replace_diagnostics(&mut self, items: Vec<LspDiagnostic>) {
        self.diagnostics_gen += 1;
        self.diagnostics = items;
    }

    fn diagnostics_version(&self) -> u64 {
        self.diagnostics_gen
    }
}
