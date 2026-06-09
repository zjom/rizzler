//! One language-server client running as a tokio task.
//!
//! Lifetime: `spawn` → `initialize` handshake (timed out at 5s) → main
//! loop that fans incoming messages out as `LspEvent`s and processes
//! outgoing `ClientCmd`s.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use crossbeam_channel::Sender as CbSender;
use futures_util::sink::SinkExt;
use futures_util::stream::StreamExt;
use lsp_types::{
    notification::{Notification as _, *},
    request::{Request as _, *},
    ClientCapabilities, ClientInfo, CompletionClientCapabilities, CompletionItemCapability,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    FormattingOptions, GeneralClientCapabilities, HoverClientCapabilities, HoverContents,
    HoverParams, InitializeParams, InitializedParams, MarkedString, MarkupKind, MessageType,
    PartialResultParams, PositionEncodingKind, PublishDiagnosticsClientCapabilities,
    PublishDiagnosticsParams, ServerCapabilities, TextDocumentClientCapabilities,
    TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, TextDocumentSyncClientCapabilities, Uri,
    VersionedTextDocumentIdentifier, WorkDoneProgressParams,
};
use rizz_actions::LspClientId;
use rizz_lsp_install::ServerSpec;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, FramedWrite};
use tracing::{debug, error, info, trace, warn};

use crate::action_bridge;
use crate::codec::LspCodec;
use crate::error::LspError;
use crate::event::{ChangeEvent, LspEvent, RequestSeq};
use crate::message::{
    IncomingMessage, OutgoingNotification, OutgoingRequest, OutgoingResponse, RequestId,
};
use crate::position::Encoding;

/// Commands the runtime sends into a client task.
#[derive(Debug)]
pub enum ClientCmd {
    DidOpen {
        uri: String,
        language_id: String,
        version: i32,
        text: String,
    },
    DidChange {
        uri: String,
        version: i32,
        changes: Vec<ChangeEvent>,
    },
    DidClose {
        uri: String,
    },
    Hover {
        seq: RequestSeq,
        uri: String,
        position: lsp_types::Position,
    },
    GotoDefinition {
        seq: RequestSeq,
        uri: String,
        position: lsp_types::Position,
    },
    Completion {
        seq: RequestSeq,
        uri: String,
        position: lsp_types::Position,
    },
    Format {
        seq: RequestSeq,
        uri: String,
        tab_size: u32,
        insert_spaces: bool,
    },
    CodeAction {
        seq: RequestSeq,
        uri: String,
        range: lsp_types::Range,
    },
    ExecuteCommand {
        seq: RequestSeq,
        command: String,
        arguments: Vec<Value>,
    },
    Cancel {
        seq: RequestSeq,
    },
    Shutdown,
}

/// Public handle a runtime dispatcher uses to talk to a running client.
pub struct ClientHandle {
    #[allow(dead_code)]
    pub id: LspClientId,
    #[allow(dead_code)]
    pub name: String,
    pub encoding: Encoding,
    pub cmd_tx: mpsc::UnboundedSender<ClientCmd>,
}

#[derive(Default)]
struct PendingRequests {
    /// Map our outbound request id → editor-side seq so we can route the
    /// response. The map's key is the JSON-RPC id we put on the wire.
    by_lsp_id: HashMap<i64, PendingKind>,
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum PendingKind {
    Hover(RequestSeq),
    GotoDefinition(RequestSeq),
    Completion(RequestSeq),
    Format(RequestSeq),
    CodeAction(RequestSeq),
    ExecuteCommand(RequestSeq),
    /// Reserved for the in-process `initialize` handshake. Never reaches
    /// the event channel.
    Initialize,
    /// `shutdown` request before sending `exit`.
    Shutdown,
}

/// Spawn the child, perform initialize, and return a handle once the
/// server is ready. The client's main loop runs in `tokio::spawn`.
pub async fn spawn(
    id: LspClientId,
    name: String,
    binary: &Path,
    spec: ServerSpec,
    root_uri: Option<String>,
    events_tx: CbSender<LspEvent>,
) -> Result<ClientHandle, LspError> {
    let mut cmd = Command::new(binary);
    cmd.args(&spec.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().map_err(|source| LspError::Spawn {
        name: name.clone(),
        command: binary.to_path_buf(),
        source,
    })?;
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<ClientCmd>();

    let mut writer = FramedWrite::new(stdin, LspCodec);
    let mut reader = FramedRead::new(stdout, LspCodec);

    // Drive stderr separately so it doesn't block stdout framing.
    let name_for_stderr = name.clone();
    tokio::spawn(drain_stderr(name_for_stderr, stderr));

    let encoding = do_initialize(&name, &spec, root_uri.clone(), &mut reader, &mut writer).await?;

    let handle = ClientHandle {
        id,
        name: name.clone(),
        encoding,
        cmd_tx,
    };

    tokio::spawn(run_main_loop(RunCtx {
        id,
        name: name.clone(),
        encoding,
        reader,
        writer,
        cmd_rx,
        events_tx,
        child,
    }));

    Ok(handle)
}

async fn do_initialize<R, W>(
    name: &str,
    spec: &ServerSpec,
    root_uri: Option<String>,
    reader: &mut FramedRead<R, LspCodec>,
    writer: &mut FramedWrite<W, LspCodec>,
) -> Result<Encoding, LspError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let root_uri_parsed = root_uri.as_deref().and_then(|s| s.parse::<Uri>().ok());
    let params = InitializeParams {
        process_id: Some(std::process::id()),
        root_uri: root_uri_parsed,
        capabilities: build_client_capabilities(),
        initialization_options: spec
            .initialization_options
            .clone()
            .and_then(|v| serde_json::to_value(v).ok()),
        client_info: Some(ClientInfo {
            name: "rizzler".to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
        ..Default::default()
    };
    let req = OutgoingRequest::new(
        RequestId::number(0),
        Initialize::METHOD,
        serde_json::to_value(params)?,
    );
    let body = serde_json::to_vec(&req)?;
    writer.send(body).await?;

    let init_response = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let Some(frame) = reader.next().await else {
                return Err(LspError::InitializeFailed {
                    name: name.to_string(),
                    message: "server closed stream during initialize".to_string(),
                });
            };
            let frame = frame?;
            let msg: IncomingMessage = serde_json::from_slice(&frame)?;
            match msg {
                IncomingMessage::Response { id, result, error, .. }
                    if id == RequestId::number(0) =>
                {
                    if let Some(err) = error {
                        return Err(LspError::InitializeFailed {
                            name: name.to_string(),
                            message: err.message,
                        });
                    }
                    return Ok(result.unwrap_or(Value::Null));
                }
                IncomingMessage::Request { method, .. } => {
                    debug!(method, "ignoring server request during initialize");
                }
                IncomingMessage::Notification { method, .. } => {
                    debug!(method, "ignoring server notification during initialize");
                }
                IncomingMessage::Response { .. } => {
                    debug!("ignoring stray response during initialize");
                }
            }
        }
    })
    .await
    .map_err(|_| LspError::InitializeTimeout {
        name: name.to_string(),
        timeout_ms: 5000,
    })??;

    // Determine encoding the server picked.
    let encoding = init_response
        .get("capabilities")
        .and_then(|c| serde_json::from_value::<ServerCapabilities>(c.clone()).ok())
        .and_then(|caps| caps.position_encoding)
        .map(|k| Encoding::from_lsp(Some(&k)))
        .unwrap_or_default();

    // Send `initialized` notification to complete the handshake.
    let notif = OutgoingNotification::new(
        Initialized::METHOD,
        serde_json::to_value(InitializedParams {})?,
    );
    writer.send(serde_json::to_vec(&notif)?).await?;
    info!(name, ?encoding, "lsp client initialized");

    Ok(encoding)
}

fn build_client_capabilities() -> ClientCapabilities {
    ClientCapabilities {
        general: Some(GeneralClientCapabilities {
            position_encodings: Some(vec![
                PositionEncodingKind::UTF8,
                PositionEncodingKind::UTF16,
            ]),
            ..Default::default()
        }),
        text_document: Some(TextDocumentClientCapabilities {
            synchronization: Some(TextDocumentSyncClientCapabilities {
                dynamic_registration: Some(false),
                will_save: Some(false),
                will_save_wait_until: Some(false),
                did_save: Some(false),
            }),
            hover: Some(HoverClientCapabilities {
                dynamic_registration: Some(false),
                content_format: Some(vec![MarkupKind::Markdown, MarkupKind::PlainText]),
            }),
            completion: Some(CompletionClientCapabilities {
                dynamic_registration: Some(false),
                completion_item: Some(CompletionItemCapability {
                    snippet_support: Some(false),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            definition: Some(Default::default()),
            formatting: Some(Default::default()),
            code_action: Some(Default::default()),
            publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                related_information: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

struct RunCtx<R: AsyncRead + Unpin + Send + 'static, W: AsyncWrite + Unpin + Send + 'static> {
    id: LspClientId,
    name: String,
    encoding: Encoding,
    reader: FramedRead<R, LspCodec>,
    writer: FramedWrite<W, LspCodec>,
    cmd_rx: mpsc::UnboundedReceiver<ClientCmd>,
    events_tx: CbSender<LspEvent>,
    child: Child,
}

const DIDCHANGE_DEBOUNCE_MS: u64 = 75;

async fn run_main_loop<R, W>(mut ctx: RunCtx<R, W>)
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut next_id: i64 = 100; // reserve [0..100) for initialize/shutdown
    let mut pending = PendingRequests::default();
    // URI → pending didChange batch. Each batch carries the latest
    // version. We send when the debounce timer fires or when a non-edit
    // command crosses the queue.
    let mut pending_changes: HashMap<String, PendingChange> = HashMap::new();
    let mut debounce = tokio::time::interval(Duration::from_millis(DIDCHANGE_DEBOUNCE_MS));
    debounce.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            biased;
            cmd = ctx.cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                let now = tokio::time::Instant::now();
                if matches!(cmd, ClientCmd::Shutdown) {
                    flush_changes(&mut ctx, &mut pending_changes).await;
                    // Initiate graceful shutdown.
                    let id = next_id; next_id += 1;
                    pending.by_lsp_id.insert(id, PendingKind::Shutdown);
                    let req = OutgoingRequest::new(
                        RequestId::number(id), Shutdown::METHOD, Value::Null,
                    );
                    let _ = send_frame(&mut ctx.writer, &req).await;
                    // Optimistically send `exit` so the server tears down.
                    let exit = OutgoingNotification::new(Exit::METHOD, Value::Null);
                    let _ = send_frame(&mut ctx.writer, &exit).await;
                    break;
                }
                if let Err(e) = handle_cmd(
                    &mut ctx, &mut next_id, &mut pending, &mut pending_changes, cmd, now,
                ).await {
                    warn!(name = %ctx.name, error = %e, "lsp command failed");
                }
            }
            frame = ctx.reader.next() => {
                match frame {
                    Some(Ok(bytes)) => {
                        if let Err(e) = handle_incoming(&mut ctx, &mut pending, &bytes).await {
                            warn!(name = %ctx.name, error = %e, "incoming message failed");
                        }
                    }
                    Some(Err(e)) => {
                        warn!(name = %ctx.name, error = %e, "framing error — reconnect not implemented");
                        break;
                    }
                    None => {
                        debug!(name = %ctx.name, "server closed stdout");
                        break;
                    }
                }
            }
            _ = debounce.tick() => {
                flush_changes(&mut ctx, &mut pending_changes).await;
            }
            exit = ctx.child.wait() => {
                let status = exit.ok().and_then(|s| s.code());
                let _ = ctx.events_tx.send(LspEvent::ServerExited {
                    client: ctx.id,
                    status,
                    stderr_tail: String::new(),
                });
                break;
            }
        }
    }
    let _ = ctx.child.start_kill();
}

struct PendingChange {
    version: i32,
    changes: Vec<ChangeEvent>,
}

async fn handle_cmd<R, W>(
    ctx: &mut RunCtx<R, W>,
    next_id: &mut i64,
    pending: &mut PendingRequests,
    pending_changes: &mut HashMap<String, PendingChange>,
    cmd: ClientCmd,
    _now: tokio::time::Instant,
) -> Result<(), LspError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    match cmd {
        ClientCmd::DidOpen {
            uri,
            language_id,
            version,
            text,
        } => {
            let params = DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: parse_uri(&uri)?,
                    language_id,
                    version,
                    text,
                },
            };
            send_notification::<DidOpenTextDocument>(&mut ctx.writer, params).await
        }
        ClientCmd::DidChange { uri, version, changes } => {
            let entry = pending_changes.entry(uri).or_insert_with(|| PendingChange {
                version,
                changes: Vec::new(),
            });
            entry.version = version;
            entry.changes.extend(changes);
            Ok(())
        }
        ClientCmd::DidClose { uri } => {
            pending_changes.remove(&uri);
            let params = DidCloseTextDocumentParams {
                text_document: TextDocumentIdentifier {
                    uri: parse_uri(&uri)?,
                },
            };
            send_notification::<DidCloseTextDocument>(&mut ctx.writer, params).await
        }
        ClientCmd::Hover { seq, uri, position } => {
            flush_changes(ctx, pending_changes).await;
            let id = *next_id; *next_id += 1;
            pending.by_lsp_id.insert(id, PendingKind::Hover(seq));
            let params = HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: parse_uri(&uri)?,
                    },
                    position,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            };
            send_request::<HoverRequest>(&mut ctx.writer, id, params).await
        }
        ClientCmd::GotoDefinition { seq, uri, position } => {
            flush_changes(ctx, pending_changes).await;
            let id = *next_id; *next_id += 1;
            pending
                .by_lsp_id
                .insert(id, PendingKind::GotoDefinition(seq));
            let params = lsp_types::GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: parse_uri(&uri)?,
                    },
                    position,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            };
            send_request::<GotoDefinition>(&mut ctx.writer, id, params).await
        }
        ClientCmd::Completion { seq, uri, position } => {
            flush_changes(ctx, pending_changes).await;
            let id = *next_id; *next_id += 1;
            pending.by_lsp_id.insert(id, PendingKind::Completion(seq));
            let params = lsp_types::CompletionParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: parse_uri(&uri)?,
                    },
                    position,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
                context: None,
            };
            send_request::<Completion>(&mut ctx.writer, id, params).await
        }
        ClientCmd::Format {
            seq,
            uri,
            tab_size,
            insert_spaces,
        } => {
            flush_changes(ctx, pending_changes).await;
            let id = *next_id; *next_id += 1;
            pending.by_lsp_id.insert(id, PendingKind::Format(seq));
            let params = lsp_types::DocumentFormattingParams {
                text_document: TextDocumentIdentifier {
                    uri: parse_uri(&uri)?,
                },
                options: FormattingOptions {
                    tab_size,
                    insert_spaces,
                    ..Default::default()
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            };
            send_request::<Formatting>(&mut ctx.writer, id, params).await
        }
        ClientCmd::CodeAction { seq, uri, range } => {
            flush_changes(ctx, pending_changes).await;
            let id = *next_id; *next_id += 1;
            pending.by_lsp_id.insert(id, PendingKind::CodeAction(seq));
            let params = lsp_types::CodeActionParams {
                text_document: TextDocumentIdentifier {
                    uri: parse_uri(&uri)?,
                },
                range,
                context: lsp_types::CodeActionContext::default(),
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            };
            send_request::<CodeActionRequest>(&mut ctx.writer, id, params).await
        }
        ClientCmd::ExecuteCommand {
            seq,
            command,
            arguments,
        } => {
            let id = *next_id; *next_id += 1;
            pending
                .by_lsp_id
                .insert(id, PendingKind::ExecuteCommand(seq));
            let params = lsp_types::ExecuteCommandParams {
                command,
                arguments,
                work_done_progress_params: WorkDoneProgressParams::default(),
            };
            send_request::<ExecuteCommand>(&mut ctx.writer, id, params).await
        }
        ClientCmd::Cancel { seq } => {
            // Find the LSP id that maps to this seq and send $/cancelRequest.
            let lsp_id = pending
                .by_lsp_id
                .iter()
                .find_map(|(id, kind)| pending_kind_seq(*kind).filter(|s| *s == seq).map(|_| *id));
            if let Some(id) = lsp_id {
                let params = json!({ "id": id });
                let notif = OutgoingNotification::new(Cancel::METHOD, params);
                send_frame(&mut ctx.writer, &notif).await?;
            }
            Ok(())
        }
        ClientCmd::Shutdown => unreachable!("shutdown handled in main loop"),
    }
}

fn pending_kind_seq(kind: PendingKind) -> Option<RequestSeq> {
    match kind {
        PendingKind::Hover(s)
        | PendingKind::GotoDefinition(s)
        | PendingKind::Completion(s)
        | PendingKind::Format(s)
        | PendingKind::CodeAction(s)
        | PendingKind::ExecuteCommand(s) => Some(s),
        PendingKind::Initialize | PendingKind::Shutdown => None,
    }
}

async fn flush_changes<R, W>(
    ctx: &mut RunCtx<R, W>,
    pending_changes: &mut HashMap<String, PendingChange>,
) where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    if pending_changes.is_empty() {
        return;
    }
    let drained: Vec<(String, PendingChange)> = pending_changes.drain().collect();
    for (uri, change) in drained {
        let Ok(parsed_uri) = uri.parse::<Uri>() else {
            warn!(uri, "skipping didChange with malformed uri");
            continue;
        };
        let params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: parsed_uri,
                version: change.version,
            },
            content_changes: change
                .changes
                .into_iter()
                .map(|c| TextDocumentContentChangeEvent {
                    range: c.range,
                    range_length: None,
                    text: c.text,
                })
                .collect(),
        };
        if let Err(e) =
            send_notification::<DidChangeTextDocument>(&mut ctx.writer, params).await
        {
            warn!(error = %e, "didChange send failed");
        }
    }
}

async fn handle_incoming<R, W>(
    ctx: &mut RunCtx<R, W>,
    pending: &mut PendingRequests,
    bytes: &[u8],
) -> Result<(), LspError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let msg: IncomingMessage = serde_json::from_slice(bytes)?;
    match msg {
        IncomingMessage::Notification { method, params, .. } => {
            handle_notification(ctx, &method, params).await
        }
        IncomingMessage::Request { id, method, params, .. } => {
            handle_request(ctx, id, &method, params).await
        }
        IncomingMessage::Response { id, result, error, .. } => {
            let RequestId::Number(n) = id else {
                trace!("ignoring string-id response");
                return Ok(());
            };
            let Some(kind) = pending.by_lsp_id.remove(&n) else {
                trace!(n, "ignoring response for unknown id");
                return Ok(());
            };
            handle_response(ctx, kind, result, error).await
        }
    }
}

async fn handle_notification<R, W>(
    ctx: &mut RunCtx<R, W>,
    method: &str,
    params: Value,
) -> Result<(), LspError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    match method {
        PublishDiagnostics::METHOD => {
            let p: PublishDiagnosticsParams = serde_json::from_value(params)?;
            // Use a per-frame empty rope; the editor side re-walks the
            // line/character positions against its own rope. We pass an
            // empty Rope here because action_bridge's diagnostic_owned
            // doesn't actually need the rope when encoding is UTF-8/UTF-16
            // — those paths only use it for end-of-line clamping. The
            // editor reapplies position translation if needed.
            let empty = ropey::Rope::new();
            let items = p
                .diagnostics
                .iter()
                .map(|d| action_bridge::diagnostic_owned(&empty, d, ctx.encoding))
                .collect();
            let _ = ctx.events_tx.send(LspEvent::Diagnostics {
                client: ctx.id,
                uri: p.uri.to_string(),
                items,
            });
        }
        ShowMessage::METHOD | LogMessage::METHOD => {
            let p: lsp_types::ShowMessageParams = serde_json::from_value(params)?;
            let _ = ctx.events_tx.send(LspEvent::Notify {
                client: ctx.id,
                kind: p.typ,
                message: p.message,
            });
        }
        _ => trace!(method, "ignoring notification"),
    }
    Ok(())
}

async fn handle_request<R, W>(
    ctx: &mut RunCtx<R, W>,
    id: RequestId,
    method: &str,
    params: Value,
) -> Result<(), LspError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    match method {
        ApplyWorkspaceEdit::METHOD => {
            let p: lsp_types::ApplyWorkspaceEditParams = serde_json::from_value(params)?;
            let edit = action_bridge::workspace_edit_owned(p.edit);
            let _ = ctx.events_tx.send(LspEvent::WorkspaceApplyEdit {
                client: ctx.id,
                edit,
            });
            // Reply optimistically. We can't synchronously await the editor.
            let resp = OutgoingResponse {
                jsonrpc: "2.0",
                id,
                result: Some(json!({ "applied": true })),
                error: None,
            };
            send_frame(&mut ctx.writer, &resp).await?;
        }
        WorkDoneProgressCreate::METHOD => {
            // Acknowledge but don't track progress.
            let resp = OutgoingResponse {
                jsonrpc: "2.0",
                id,
                result: Some(Value::Null),
                error: None,
            };
            send_frame(&mut ctx.writer, &resp).await?;
        }
        _ => {
            // Politely refuse so the server doesn't hang on us.
            let resp = OutgoingResponse {
                jsonrpc: "2.0",
                id,
                result: None,
                error: Some(crate::message::ResponseError {
                    code: -32601,
                    message: format!("rizz_lsp: method `{method}` not implemented"),
                }),
            };
            send_frame(&mut ctx.writer, &resp).await?;
        }
    }
    Ok(())
}

async fn handle_response<R, W>(
    ctx: &mut RunCtx<R, W>,
    kind: PendingKind,
    result: Option<Value>,
    error: Option<crate::message::ResponseError>,
) -> Result<(), LspError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    if let Some(err) = error {
        if let Some(seq) = pending_kind_seq(kind) {
            let _ = ctx.events_tx.send(LspEvent::RequestError {
                client: ctx.id,
                seq,
                message: err.message,
            });
        }
        return Ok(());
    }
    let result = result.unwrap_or(Value::Null);
    let empty = ropey::Rope::new();
    match kind {
        PendingKind::Hover(seq) => {
            let parsed: Option<lsp_types::Hover> =
                serde_json::from_value(result).ok().flatten();
            let contents = parsed.map(|h| render_hover(h.contents));
            let _ = ctx.events_tx.send(LspEvent::HoverResponse {
                client: ctx.id,
                seq,
                contents,
            });
        }
        PendingKind::GotoDefinition(seq) => {
            let resp: Option<lsp_types::GotoDefinitionResponse> =
                serde_json::from_value(result).ok().flatten();
            let locations = resp
                .map(|r| action_bridge::locations_owned(&empty, r, ctx.encoding))
                .unwrap_or_default();
            let _ = ctx.events_tx.send(LspEvent::DefinitionResponse {
                client: ctx.id,
                seq,
                locations,
            });
        }
        PendingKind::Completion(seq) => {
            let resp: Option<lsp_types::CompletionResponse> =
                serde_json::from_value(result).ok().flatten();
            let items = resp.map(action_bridge::completion_items).unwrap_or_default();
            let _ = ctx.events_tx.send(LspEvent::CompletionResponse {
                client: ctx.id,
                seq,
                items,
            });
        }
        PendingKind::Format(seq) => {
            let edits: Option<Vec<lsp_types::TextEdit>> = serde_json::from_value(result).ok();
            let edits = action_bridge::text_edits_owned(edits.unwrap_or_default());
            let _ = ctx.events_tx.send(LspEvent::FormattingResponse {
                client: ctx.id,
                seq,
                edits,
            });
        }
        PendingKind::CodeAction(seq) => {
            let items: Option<Vec<lsp_types::CodeActionOrCommand>> =
                serde_json::from_value(result).ok();
            let actions = action_bridge::code_actions_owned(items.unwrap_or_default());
            let _ = ctx.events_tx.send(LspEvent::CodeActionResponse {
                client: ctx.id,
                seq,
                actions,
            });
        }
        PendingKind::ExecuteCommand(_seq) => {
            // No response payload to forward.
        }
        PendingKind::Initialize | PendingKind::Shutdown => {
            // Should not be reached after the initialize handshake.
        }
    }
    Ok(())
}

fn render_hover(contents: HoverContents) -> String {
    match contents {
        HoverContents::Scalar(MarkedString::String(s)) => s,
        HoverContents::Scalar(MarkedString::LanguageString(ls)) => ls.value,
        HoverContents::Array(items) => items
            .into_iter()
            .map(|m| match m {
                MarkedString::String(s) => s,
                MarkedString::LanguageString(ls) => ls.value,
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        HoverContents::Markup(m) => m.value,
    }
}

async fn send_request<R: lsp_types::request::Request>(
    writer: &mut FramedWrite<impl AsyncWrite + Unpin, LspCodec>,
    id: i64,
    params: R::Params,
) -> Result<(), LspError>
where
    R::Params: serde::Serialize,
{
    let req = OutgoingRequest::new(
        RequestId::number(id),
        R::METHOD,
        serde_json::to_value(params)?,
    );
    send_frame(writer, &req).await
}

async fn send_notification<N: lsp_types::notification::Notification>(
    writer: &mut FramedWrite<impl AsyncWrite + Unpin, LspCodec>,
    params: N::Params,
) -> Result<(), LspError>
where
    N::Params: serde::Serialize,
{
    let notif = OutgoingNotification::new(N::METHOD, serde_json::to_value(params)?);
    send_frame(writer, &notif).await
}

async fn send_frame<T: serde::Serialize>(
    writer: &mut FramedWrite<impl AsyncWrite + Unpin, LspCodec>,
    value: &T,
) -> Result<(), LspError> {
    let body = serde_json::to_vec(value)?;
    writer.send(body).await
}

fn parse_uri(s: &str) -> Result<Uri, LspError> {
    s.parse::<Uri>().map_err(|e| LspError::Frame {
        reason: format!("bad uri {s:?}: {e}"),
    })
}

async fn drain_stderr(name: String, mut stderr: tokio::process::ChildStderr) {
    use tokio::io::AsyncReadExt;
    let mut buf = [0u8; 4096];
    loop {
        match stderr.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let s = String::from_utf8_lossy(&buf[..n]);
                for line in s.lines() {
                    trace!(name = %name, "lsp stderr: {line}");
                }
            }
            Err(e) => {
                error!(name = %name, error = %e, "stderr read failed");
                break;
            }
        }
    }
}
