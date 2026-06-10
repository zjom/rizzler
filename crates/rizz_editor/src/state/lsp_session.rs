//! In-flight LSP session state + the per-tick event drain.
//!
//! `install_lsp_client` in [`super::lang`] spawns the server and attaches it
//! to a buffer. The outgoing request side lives in [`super::lsp_requests`];
//! the response/display side lives in [`super::lsp_responses`]. This module
//! owns the bookkeeping both share — the pending-request map, sequence
//! counter, response callbacks — and the drain that converts each incoming
//! [`LspEvent`] back into follow-up `Action`s.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use rizz::runtime::Value;
use rizz_actions::Action;
use rizz_core::FilePos;
use rizz_lsp::{LspEvent, RequestSeq};
use rizz_text::BufferId;
use tracing::{debug, instrument, warn};

use super::State;

/// Side-table backing `(lsp-apply-completion id)`. Captures the originating
/// buffer and anchor so a delayed invocation still applies in the right
/// place even if the cursor has moved.
#[derive(Debug, Clone)]
pub(crate) struct PendingCompletion {
    pub buf: BufferId,
    pub anchor: FilePos,
    pub items: Arc<[rizz_actions::CompletionItemOwned]>,
}

/// Side-table backing `(lsp-invoke-code-action id)`. The buffer field is
/// used to look up the originating LSP client at invoke time when the
/// action carries a `Command` instead of an edit.
#[derive(Debug, Clone)]
pub(crate) struct PendingCodeActions {
    pub buf: BufferId,
    pub actions: Arc<[rizz_actions::CodeActionOwned]>,
}

/// What to do with an LSP response when it lands. Held in
/// `LspSession::pending_requests` so the asynchronous drain can route each
/// reply back to the originating buffer / cursor anchor.
#[derive(Debug, Clone)]
#[allow(dead_code)] // some fields exist for future routing logic
pub(super) enum PendingLspKind {
    Hover {
        buf: BufferId,
        anchor: FilePos,
    },
    GotoDefinition {
        buf: BufferId,
    },
    Completion {
        buf: BufferId,
        anchor: FilePos,
    },
    Format {
        buf: BufferId,
        deadline: Instant,
    },
    CodeAction {
        buf: BufferId,
    },
}

/// In-flight LSP request bookkeeping. Tracks pending requests (so responses
/// can route back to the originating buffer + cursor), the sequence
/// counter, the lisp callbacks for completion + code-action responses, and
/// the most-recent batches needed by `(lsp-apply-completion id)` /
/// `(lsp-invoke-code-action id)`.
pub(super) struct LspSession {
    pub pending_requests: HashMap<RequestSeq, PendingLspKind>,
    pub next_seq: RequestSeq,
    pub completion_fn: Option<Rc<Value>>,
    pub code_action_fn: Option<Rc<Value>>,
    pub pending_completion: Option<PendingCompletion>,
    pub pending_code_actions: Option<PendingCodeActions>,
}

impl LspSession {
    pub(super) fn new() -> Self {
        Self {
            pending_requests: HashMap::new(),
            next_seq: 1,
            completion_fn: None,
            code_action_fn: None,
            pending_completion: None,
            pending_code_actions: None,
        }
    }

    /// Hand out the next request sequence and bump the counter. Wraps on
    /// overflow — collisions only matter for in-flight requests, and the
    /// pending-request map is bounded by the editor's concurrent request
    /// count, well below `RequestSeq::MAX`.
    pub(super) fn alloc_seq(&mut self) -> RequestSeq {
        let s = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        s
    }
}

impl State {
    /// Install the lisp callback for `textDocument/completion` responses.
    /// The fn receives `(items anchor)` — `items` is an array of maps with
    /// fields `id`, `label`, `detail`, `insert-text`, `kind`; `anchor` is a
    /// map with `row` / `col`. Pass `None` to revert to the notify fallback.
    pub fn set_lsp_completion_fn(&mut self, f: Option<Rc<Value>>) {
        self.lsp_session.completion_fn = f;
    }

    pub fn lsp_completion_fn(&self) -> Option<&Rc<Value>> {
        self.lsp_session.completion_fn.as_ref()
    }

    /// Install the lisp callback for `textDocument/codeAction` responses.
    /// The fn receives `(actions)` — an array of maps with fields `id`,
    /// `title`, `kind`, `has-edit`, `has-command`.
    pub fn set_lsp_code_action_fn(&mut self, f: Option<Rc<Value>>) {
        self.lsp_session.code_action_fn = f;
    }

    pub fn lsp_code_action_fn(&self) -> Option<&Rc<Value>> {
        self.lsp_session.code_action_fn.as_ref()
    }

    /// Drain pending LSP events and convert each to follow-up `Action`s.
    /// Called from the editor's main loop on every tick.
    #[instrument(skip(self))]
    pub fn drain_lsp_events(&mut self) -> Vec<Rc<Action>> {
        let mut out: Vec<Rc<Action>> = Vec::new();
        let rx = rizz_lsp::runtime().events_rx().clone();
        while let Ok(ev) = rx.try_recv() {
            debug!(event = lsp_event_name(&ev), "lsp event");
            self.handle_lsp_event(ev, &mut out);
        }
        out
    }

    pub(super) fn handle_lsp_event(&mut self, ev: LspEvent, out: &mut Vec<Rc<Action>>) {
        match ev {
            LspEvent::Diagnostics { uri, items, .. } => {
                if let Some(bid) = self.bufs.id_for_uri(&uri)
                    && let Some(b) = self.bufs.get_mut(bid)
                    && let Some(h) = b.lsp_handle_mut()
                {
                    h.replace_diagnostics(items);
                }
            }
            LspEvent::HoverResponse { seq, contents, .. } => {
                let Some(PendingLspKind::Hover { anchor, .. }) =
                    self.lsp_session.pending_requests.remove(&seq)
                else {
                    return;
                };
                let contents = contents.unwrap_or_default();
                if !contents.is_empty() {
                    out.push(Rc::new(Action::LspShowHover {
                        contents: Arc::from(contents.as_str()),
                        anchor,
                    }));
                }
            }
            LspEvent::DefinitionResponse { seq, locations, .. } => {
                if self.lsp_session.pending_requests.remove(&seq).is_none() {
                    return;
                }
                if locations.is_empty() {
                    out.push(Rc::new(Action::LspShowDefinitionList {
                        locations: Arc::from([]),
                    }));
                } else {
                    out.push(Rc::new(Action::LspShowDefinitionList {
                        locations: Arc::from(locations),
                    }));
                }
            }
            LspEvent::CompletionResponse { seq, items, .. } => {
                let Some(PendingLspKind::Completion { anchor, .. }) =
                    self.lsp_session.pending_requests.remove(&seq)
                else {
                    return;
                };
                out.push(Rc::new(Action::LspShowCompletion {
                    items: Arc::from(items),
                    anchor,
                }));
            }
            LspEvent::FormattingResponse { seq, edits, .. } => {
                let Some(PendingLspKind::Format { buf, deadline }) =
                    self.lsp_session.pending_requests.remove(&seq)
                else {
                    return;
                };
                if Instant::now() > deadline {
                    self.notify_via_lisp("lsp format: timed out");
                    return;
                }
                out.push(Rc::new(Action::LspApplyTextEdits {
                    buf,
                    edits: Arc::from(edits),
                    label: Arc::from("lsp format"),
                }));
            }
            LspEvent::CodeActionResponse { seq, actions, .. } => {
                if self.lsp_session.pending_requests.remove(&seq).is_none() {
                    return;
                }
                out.push(Rc::new(Action::LspShowCodeActions {
                    actions: Arc::from(actions),
                }));
            }
            LspEvent::WorkspaceApplyEdit { edit, .. } => {
                out.push(Rc::new(Action::LspApplyWorkspaceEdit {
                    edit: Arc::new(edit),
                    label: Arc::from("workspace edit"),
                }));
            }
            LspEvent::WorkspaceExecuteCommand { client, command } => {
                out.push(Rc::new(Action::LspExecuteCommand { client, command }));
            }
            LspEvent::ServerExited {
                client,
                status,
                stderr_tail,
            } => {
                self.lang.lsp_registry.forget(client);
                self.notify_via_lisp(&format!(
                    "lsp server exited (status {status:?}): {stderr_tail}",
                ));
            }
            LspEvent::Notify { kind, message, .. } => {
                debug!(?kind, %message, "lsp notify");
                if !message.is_empty() {
                    self.notify_via_lisp(&message);
                }
            }
            LspEvent::RequestError { seq, message, .. } => {
                self.lsp_session.pending_requests.remove(&seq);
                warn!(seq, %message, "lsp request error");
            }
        }
    }

    /// Drain any pending LSP events and apply the synthesized actions.
    /// Call this from the main loop after every event tick (and on the
    /// `event::poll` timeout branch) so server-pushed diagnostics surface
    /// even when the user is idle. Returns whether anything changed (the
    /// caller should re-render).
    #[instrument(skip(self))]
    pub fn tick(&mut self) -> bool {
        let actions = self.drain_lsp_events();
        if actions.is_empty() {
            return false;
        }
        self.apply(&actions);
        true
    }
}

/// Variant name for drain-time logging, without dumping the payload.
fn lsp_event_name(ev: &LspEvent) -> &'static str {
    match ev {
        LspEvent::Diagnostics { .. } => "Diagnostics",
        LspEvent::HoverResponse { .. } => "HoverResponse",
        LspEvent::DefinitionResponse { .. } => "DefinitionResponse",
        LspEvent::CompletionResponse { .. } => "CompletionResponse",
        LspEvent::FormattingResponse { .. } => "FormattingResponse",
        LspEvent::CodeActionResponse { .. } => "CodeActionResponse",
        LspEvent::WorkspaceApplyEdit { .. } => "WorkspaceApplyEdit",
        LspEvent::WorkspaceExecuteCommand { .. } => "WorkspaceExecuteCommand",
        LspEvent::ServerExited { .. } => "ServerExited",
        LspEvent::Notify { .. } => "Notify",
        LspEvent::RequestError { .. } => "RequestError",
    }
}
