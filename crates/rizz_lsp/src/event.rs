//! Cross-thread event types: editor → tokio (commands) and tokio →
//! editor (responses + notifications). All ownership-transferable so they
//! cross the `crossbeam_channel` cleanly.

use std::path::PathBuf;

use crossbeam_channel::Sender;
use rizz_actions::{
    CodeActionOwned, CommandOwned, CompletionItemOwned, LocationOwned, LspClientId, TextEditOwned,
    WorkspaceEditOwned,
};
use rizz_core::LspDiagnostic;
use rizz_lsp_install::ServerSpec;
use serde_json::Value;

use crate::position::Encoding;

/// Stable identifier for an in-flight request, assigned by the editor and
/// echoed back on the corresponding response event.
pub type RequestSeq = u64;

#[derive(Debug)]
pub enum RuntimeCmd {
    /// Spawn a new client task. The runtime replies on `reply` once
    /// `initialize` succeeds; the editor blocks briefly on this — the
    /// handshake is fast.
    SpawnClient {
        name: String,
        binary: PathBuf,
        spec: ServerSpec,
        root_uri: Option<String>,
        reply: Sender<SpawnReply>,
    },
    DidOpen {
        client: LspClientId,
        uri: String,
        language_id: String,
        version: i32,
        text: String,
    },
    /// Queue an incremental change. The client task coalesces these and
    /// emits one `textDocument/didChange` per debounce window.
    DidChange {
        client: LspClientId,
        uri: String,
        version: i32,
        changes: Vec<ChangeEvent>,
    },
    DidClose {
        client: LspClientId,
        uri: String,
    },
    Hover {
        client: LspClientId,
        seq: RequestSeq,
        uri: String,
        position: lsp_types::Position,
    },
    GotoDefinition {
        client: LspClientId,
        seq: RequestSeq,
        uri: String,
        position: lsp_types::Position,
    },
    Completion {
        client: LspClientId,
        seq: RequestSeq,
        uri: String,
        position: lsp_types::Position,
    },
    Format {
        client: LspClientId,
        seq: RequestSeq,
        uri: String,
        tab_size: u32,
        insert_spaces: bool,
    },
    CodeAction {
        client: LspClientId,
        seq: RequestSeq,
        uri: String,
        range: lsp_types::Range,
    },
    ExecuteCommand {
        client: LspClientId,
        seq: RequestSeq,
        command: String,
        arguments: Vec<Value>,
    },
    Cancel {
        client: LspClientId,
        seq: RequestSeq,
    },
    /// Graceful shutdown: the runtime drives `shutdown` → `exit` and reaps
    /// the child.
    Shutdown {
        client: LspClientId,
    },
}

#[derive(Debug)]
pub enum SpawnReply {
    Ok {
        client: LspClientId,
        encoding: Encoding,
    },
    Err(String),
}

#[derive(Debug, Clone)]
pub struct ChangeEvent {
    pub range: Option<lsp_types::Range>,
    pub text: String,
}

#[derive(Debug, Clone)]
pub enum LspEvent {
    Diagnostics {
        client: LspClientId,
        uri: String,
        items: Vec<LspDiagnostic>,
    },
    HoverResponse {
        client: LspClientId,
        seq: RequestSeq,
        contents: Option<String>,
    },
    DefinitionResponse {
        client: LspClientId,
        seq: RequestSeq,
        locations: Vec<LocationOwned>,
    },
    CompletionResponse {
        client: LspClientId,
        seq: RequestSeq,
        items: Vec<CompletionItemOwned>,
    },
    FormattingResponse {
        client: LspClientId,
        seq: RequestSeq,
        edits: Vec<TextEditOwned>,
    },
    CodeActionResponse {
        client: LspClientId,
        seq: RequestSeq,
        actions: Vec<CodeActionOwned>,
    },
    /// Server-initiated `workspace/applyEdit`.
    WorkspaceApplyEdit {
        client: LspClientId,
        edit: WorkspaceEditOwned,
    },
    /// Server-initiated `workspace/executeCommand`.
    WorkspaceExecuteCommand {
        client: LspClientId,
        command: CommandOwned,
    },
    ServerExited {
        client: LspClientId,
        status: Option<i32>,
        stderr_tail: String,
    },
    /// `window/showMessage` or `window/logMessage` from the server.
    Notify {
        client: LspClientId,
        kind: lsp_types::MessageType,
        message: String,
    },
    RequestError {
        client: LspClientId,
        seq: RequestSeq,
        message: String,
    },
}
