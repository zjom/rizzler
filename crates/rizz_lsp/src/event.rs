//! Cross-thread event types: editor → tokio (commands) and tokio →
//! editor (responses + notifications). All ownership-transferable so they
//! cross the `crossbeam_channel` cleanly.

use std::path::PathBuf;

use crossbeam_channel::Sender;
use rizz_actions::{
    CodeActionOwned, CommandOwned, CompletionItemOwned, LocationOwned, LspClientId,
    TextEditOwned, WorkspaceEditOwned,
};
use rizz_core::LspDiagnostic;
use rizz_lsp_install::ServerSpec;
use serde_json::Value;

use crate::position::Encoding;

/// Stable identifier for an in-flight request. The editor side stores its
/// own routing state keyed by this; the tokio side uses it to match
/// responses.
pub type RequestSeq = u64;

#[derive(Debug)]
pub enum RuntimeCmd {
    /// Spawn a new client task. The runtime replies with the assigned
    /// `LspClientId` on `reply` once `initialize` succeeds. The editor
    /// side blocks the apply loop briefly here — initialize is fast.
    SpawnClient {
        name: String,
        binary: PathBuf,
        spec: ServerSpec,
        root_uri: Option<String>,
        reply: Sender<SpawnReply>,
    },
    /// Send a `textDocument/didOpen` for `uri` with this version + text.
    DidOpen {
        client: LspClientId,
        uri: String,
        language_id: String,
        version: i32,
        text: String,
    },
    /// Queue an incremental change. The client task debounces these and
    /// emits a single `textDocument/didChange` per debounce window.
    DidChange {
        client: LspClientId,
        uri: String,
        version: i32,
        changes: Vec<ChangeEvent>,
    },
    /// Send `textDocument/didClose`.
    DidClose {
        client: LspClientId,
        uri: String,
    },
    /// Send `textDocument/hover` at `pos` (LSP coordinates already converted).
    Hover {
        client: LspClientId,
        seq: RequestSeq,
        uri: String,
        position: lsp_types::Position,
    },
    /// Send `textDocument/definition` at `pos`.
    GotoDefinition {
        client: LspClientId,
        seq: RequestSeq,
        uri: String,
        position: lsp_types::Position,
    },
    /// Send `textDocument/completion` at `pos`.
    Completion {
        client: LspClientId,
        seq: RequestSeq,
        uri: String,
        position: lsp_types::Position,
    },
    /// Send `textDocument/formatting`.
    Format {
        client: LspClientId,
        seq: RequestSeq,
        uri: String,
        tab_size: u32,
        insert_spaces: bool,
    },
    /// Send `textDocument/codeAction` for `range`.
    CodeAction {
        client: LspClientId,
        seq: RequestSeq,
        uri: String,
        range: lsp_types::Range,
    },
    /// Send `workspace/executeCommand`.
    ExecuteCommand {
        client: LspClientId,
        seq: RequestSeq,
        command: String,
        arguments: Vec<Value>,
    },
    /// Cancel an in-flight request via `$/cancelRequest`.
    Cancel {
        client: LspClientId,
        seq: RequestSeq,
    },
    /// Shut down the client task cleanly. The runtime drives `shutdown`
    /// → `exit` and reaps the child process.
    Shutdown { client: LspClientId },
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
    /// Server pushed `textDocument/publishDiagnostics` for `uri`.
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
    /// Server requested that we apply a workspace edit.
    WorkspaceApplyEdit {
        client: LspClientId,
        edit: WorkspaceEditOwned,
    },
    /// Server requested that we run one of its commands.
    WorkspaceExecuteCommand {
        client: LspClientId,
        command: CommandOwned,
    },
    /// Server crashed or exited unexpectedly.
    ServerExited {
        client: LspClientId,
        status: Option<i32>,
        stderr_tail: String,
    },
    /// Server-initiated message we want to surface as a notify.
    Notify {
        client: LspClientId,
        kind: lsp_types::MessageType,
        message: String,
    },
    /// Request error from the server (e.g., hover returned an error).
    RequestError {
        client: LspClientId,
        seq: RequestSeq,
        message: String,
    },
}
