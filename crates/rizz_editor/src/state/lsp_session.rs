//! In-flight LSP session: pending request map, sequence counter, response
//! callbacks, and the per-tick event drain.
//!
//! `install_lsp_client` in [`super::lang`] spawns the server and attaches it
//! to a buffer. From there, this module routes `(buf, anchor)` context into
//! each outgoing request and dispatches the response back to the originating
//! buffer (the buffer/cursor may have moved between request and reply).

use std::io;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rizz::runtime::Value;
use rizz_actions::{Action, LspClientId};
use rizz_core::Position;
use rizz_lsp::{
    Encoding as LspEncoding, LspEvent, RequestSeq, RuntimeCmd,
};
use rizz_text::BufferId;
use tracing::{debug, instrument, warn};

use super::{workspace::uri_to_path, State};

pub(super) const LSP_FORMAT_TIMEOUT: Duration = Duration::from_millis(2000);

/// Side-table backing `(lsp-apply-completion id)`. Captures the originating
/// buffer and anchor so a delayed invocation still applies in the right
/// place even if the cursor has moved.
#[derive(Debug, Clone)]
pub(crate) struct PendingCompletion {
    pub buf: BufferId,
    pub anchor: Position<usize>,
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
/// `State::pending_lsp_requests` so the asynchronous drain can route each
/// reply back to the originating buffer / cursor anchor.
#[derive(Debug, Clone)]
#[allow(dead_code)] // some fields exist for future routing logic
pub(super) enum PendingLspKind {
    Hover {
        buf: BufferId,
        anchor: Position<usize>,
    },
    GotoDefinition {
        buf: BufferId,
    },
    Completion {
        buf: BufferId,
        anchor: Position<usize>,
    },
    Format {
        buf: BufferId,
        deadline: Instant,
    },
    CodeAction {
        buf: BufferId,
    },
}

impl State {
    /// Install the lisp callback for `textDocument/completion` responses.
    /// The fn receives `(items anchor)` — `items` is an array of maps with
    /// fields `id`, `label`, `detail`, `insert-text`, `kind`; `anchor` is a
    /// map with `row` / `col`. Pass `None` to revert to the notify fallback.
    pub fn set_lsp_completion_fn(&mut self, f: Option<Rc<Value>>) {
        self.lsp_completion_fn = f;
    }

    pub fn lsp_completion_fn(&self) -> Option<&Rc<Value>> {
        self.lsp_completion_fn.as_ref()
    }

    /// Install the lisp callback for `textDocument/codeAction` responses.
    /// The fn receives `(actions)` — an array of maps with fields `id`,
    /// `title`, `kind`, `has-edit`, `has-command`.
    pub fn set_lsp_code_action_fn(&mut self, f: Option<Rc<Value>>) {
        self.lsp_code_action_fn = f;
    }

    pub fn lsp_code_action_fn(&self) -> Option<&Rc<Value>> {
        self.lsp_code_action_fn.as_ref()
    }

    pub(super) fn next_lsp_seq(&mut self) -> RequestSeq {
        let s = self.next_lsp_seq;
        self.next_lsp_seq = self.next_lsp_seq.wrapping_add(1);
        s
    }

    /// Compute the LSP `Position` for the focused buffer's cursor, and
    /// return `(client, uri, encoding, position, abs_pos)`. Returns
    /// `None` if no LSP attachment is present.
    pub(super) fn focused_lsp_context(
        &self,
        buf: BufferId,
    ) -> Option<(
        LspClientId,
        String,
        LspEncoding,
        lsp_types::Position,
        Position<usize>,
    )> {
        let b = self.bufs.get(buf)?;
        let handle = b.lsp_handle()?;
        // The trait-object attachment can't be downcast for client+encoding,
        // so we look it up via `buf_by_uri` (every attached buffer is
        // registered there) and the manifest entry for the file's extension.
        let uri = self
            .buf_by_uri
            .iter()
            .find_map(|(uri, bid)| if *bid == buf { Some(uri.clone()) } else { None })?;
        let _ = handle;
        let _attach_marker = b.diagnostics();
        let abs = b.abs_pos();
        let ext = b.fs_path().and_then(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_ascii_lowercase())
        })?;
        let name = self.lang.lsp.manifest.lookup_by_ext(&ext)?;
        let running = self.lang.lsp_registry.get(name)?;
        let lsp_pos = rizz_lsp::byte_to_lsp(b.rope(), abs.row, abs.col, running.encoding);
        Some((running.id, uri, running.encoding, lsp_pos, abs))
    }

    pub(crate) fn lsp_send_hover_focused(&mut self) {
        let buf = self.focused_buf_id();
        let Some((client, uri, _enc, position, anchor)) = self.focused_lsp_context(buf) else {
            self.notify_via_lisp("lsp hover: no language server attached to this buffer");
            return;
        };
        let seq = self.next_lsp_seq();
        self.pending_lsp_requests
            .insert(seq, PendingLspKind::Hover { buf, anchor });
        rizz_lsp::runtime().send_cmd(RuntimeCmd::Hover {
            client,
            seq,
            uri,
            position,
        });
    }

    pub(crate) fn lsp_send_goto_definition_focused(&mut self) {
        let buf = self.focused_buf_id();
        let Some((client, uri, _enc, position, _anchor)) = self.focused_lsp_context(buf) else {
            self.notify_via_lisp("lsp goto-definition: no language server attached to this buffer");
            return;
        };
        let seq = self.next_lsp_seq();
        self.pending_lsp_requests
            .insert(seq, PendingLspKind::GotoDefinition { buf });
        rizz_lsp::runtime().send_cmd(RuntimeCmd::GotoDefinition {
            client,
            seq,
            uri,
            position,
        });
    }

    pub(crate) fn lsp_send_completion_focused(&mut self) {
        let buf = self.focused_buf_id();
        let Some((client, uri, _enc, position, anchor)) = self.focused_lsp_context(buf) else {
            self.notify_via_lisp("lsp completion: no language server attached to this buffer");
            return;
        };
        let seq = self.next_lsp_seq();
        self.pending_lsp_requests
            .insert(seq, PendingLspKind::Completion { buf, anchor });
        rizz_lsp::runtime().send_cmd(RuntimeCmd::Completion {
            client,
            seq,
            uri,
            position,
        });
    }

    pub(crate) fn lsp_send_format_focused(&mut self) {
        let buf = self.focused_buf_id();
        let Some((client, uri, _enc, _position, _anchor)) = self.focused_lsp_context(buf) else {
            self.notify_via_lisp("lsp format: no language server attached to this buffer");
            return;
        };
        let seq = self.next_lsp_seq();
        let deadline = Instant::now() + LSP_FORMAT_TIMEOUT;
        self.pending_lsp_requests
            .insert(seq, PendingLspKind::Format { buf, deadline });
        rizz_lsp::runtime().send_cmd(RuntimeCmd::Format {
            client,
            seq,
            uri,
            tab_size: 4,
            insert_spaces: true,
        });
    }

    pub(crate) fn lsp_send_code_action_focused(&mut self) {
        let buf = self.focused_buf_id();
        let Some((client, uri, _enc, position, _anchor)) = self.focused_lsp_context(buf) else {
            self.notify_via_lisp("lsp code-action: no language server attached to this buffer");
            return;
        };
        let seq = self.next_lsp_seq();
        // TODO: support visual-selection-driven ranges.
        let range = lsp_types::Range {
            start: position,
            end: position,
        };
        self.pending_lsp_requests
            .insert(seq, PendingLspKind::CodeAction { buf });
        rizz_lsp::runtime().send_cmd(RuntimeCmd::CodeAction {
            client,
            seq,
            uri,
            range,
        });
    }

    pub(crate) fn lsp_send_did_open_focused(&mut self) {
        // `set_lsp_handle` already fires `did_open` on install; this hook
        // exists for future re-open paths.
    }

    pub(crate) fn lsp_send_did_close_focused(&mut self) {
        let buf = self.focused_buf_id();
        if let Some(b) = self.bufs.get_mut(buf) {
            b.set_lsp_handle(None);
        }
        self.buf_by_uri.retain(|_, bid| *bid != buf);
    }

    pub(crate) fn lsp_restart(&mut self, name: Option<&str>) {
        let buf = self.focused_buf_id();
        let resolved_name = name.map(str::to_string).or_else(|| {
            self.bufs
                .get(buf)
                .and_then(|b| b.fs_path())
                .and_then(|p| {
                    p.extension()
                        .and_then(|e| e.to_str())
                        .map(|s| s.to_ascii_lowercase())
                })
                .and_then(|ext| self.lang.lsp.manifest.lookup_by_ext(&ext).map(str::to_string))
        });
        let Some(server_name) = resolved_name else {
            self.notify_via_lisp(
                "lsp restart: no server name and no LSP attached to focused buffer",
            );
            return;
        };
        // Detach every buffer using this server and shut down the client;
        // the next buffer open re-spawns it.
        let uris: Vec<String> = self.buf_by_uri.keys().cloned().collect();
        for uri in &uris {
            if let Some(&bid) = self.buf_by_uri.get(uri)
                && let Some(b) = self.bufs.get_mut(bid)
            {
                b.set_lsp_handle(None);
            }
        }
        self.buf_by_uri.clear();
        self.lang.lsp_registry.shutdown(&server_name);
        self.notify_via_lisp(&format!("lsp `{server_name}` restarted"));
        self.install_lsp_client(buf);
    }

    pub(crate) fn show_lsp_hover(&mut self, contents: Arc<str>, _anchor: Position<usize>) {
        // TODO: surface as a floating overlay instead of a notify.
        let s: &str = &contents;
        if !s.is_empty() {
            self.notify_via_lisp(&format!("hover: {s}"));
        }
    }

    pub(crate) fn show_lsp_definition_list(
        &mut self,
        locations: Arc<[rizz_actions::LocationOwned]>,
    ) {
        if locations.is_empty() {
            self.notify_via_lisp("no definition found");
            return;
        }
        let target = &locations[0];
        let Some(path) = uri_to_path(&target.uri) else {
            self.notify_via_lisp(&format!(
                "definition target `{}` is not a local file",
                target.uri
            ));
            return;
        };
        // A brand-new buffer has viewport `(0,0)` until layout runs, and
        // cursor clamping / centering / syntax highlighting all bail when
        // viewport row is zero. Refresh viewports first so the landing +
        // centering operate on real dimensions.
        let dest_buf = self.open_or_focus_file(&path);
        self.refresh_viewport();
        if let Some(b) = self.bufs.get_mut(dest_buf) {
            let row = target.range.start.row;
            let col = target.range.start.col;
            b.land_cursor_to(row, col);
            b.move_cursor(rizz_text::MoveKind::Center);
        }
        if locations.len() > 1 {
            self.notify_via_lisp(&format!(
                "jumped to first of {} definitions",
                locations.len()
            ));
        }
    }

    pub(crate) fn show_lsp_completion(
        &mut self,
        items: Arc<[rizz_actions::CompletionItemOwned]>,
        anchor: Position<usize>,
    ) {
        if items.is_empty() {
            self.lsp_pending_completion = None;
            // Hand `[]` to the callback so the lisp side can dismiss any
            // open completion popup.
            if self.lsp_completion_fn.is_some() {
                self.fire_lsp_completion_fn(&items, anchor);
            } else {
                self.notify_via_lisp("no completions");
            }
            return;
        }
        let buf = self.focused_buf_id();
        self.lsp_pending_completion = Some(PendingCompletion {
            buf,
            anchor,
            items: items.clone(),
        });
        if self.lsp_completion_fn.is_some() {
            self.fire_lsp_completion_fn(&items, anchor);
        } else {
            self.notify_via_lisp(&format!(
                "completion: {} ({} more)",
                items[0].label,
                items.len().saturating_sub(1)
            ));
        }
    }

    fn fire_lsp_completion_fn(
        &mut self,
        items: &Arc<[rizz_actions::CompletionItemOwned]>,
        anchor: Position<usize>,
    ) {
        use crate::lisp::lsp_convert::{completion_items_to_value, position_to_value};
        let Some(f) = self.lsp_completion_fn.clone() else {
            return;
        };
        let items_val = completion_items_to_value(items);
        let anchor_val = position_to_value(anchor);
        let res = self.with_lisp(|lisp| {
            let env = lisp.env().clone();
            rizz::runtime::apply(&f, &[items_val, anchor_val], &env)
        });
        if let Err(e) = res {
            let msg = format!("lsp-completion-fn failed: {e}");
            warn!(error = %e, "lsp completion callback failed");
            self.notify_via_lisp(&msg);
        }
    }

    pub(crate) fn show_lsp_code_actions(&mut self, actions: Arc<[rizz_actions::CodeActionOwned]>) {
        if actions.is_empty() {
            self.lsp_pending_code_actions = None;
            if self.lsp_code_action_fn.is_some() {
                self.fire_lsp_code_action_fn(&actions);
            } else {
                self.notify_via_lisp("no code actions");
            }
            return;
        }
        let buf = self.focused_buf_id();
        self.lsp_pending_code_actions = Some(PendingCodeActions {
            buf,
            actions: actions.clone(),
        });
        if self.lsp_code_action_fn.is_some() {
            self.fire_lsp_code_action_fn(&actions);
        } else {
            self.notify_via_lisp(&format!(
                "code action: {} ({} more)",
                actions[0].title,
                actions.len().saturating_sub(1)
            ));
        }
    }

    fn fire_lsp_code_action_fn(&mut self, actions: &Arc<[rizz_actions::CodeActionOwned]>) {
        use crate::lisp::lsp_convert::code_actions_to_value;
        let Some(f) = self.lsp_code_action_fn.clone() else {
            return;
        };
        let actions_val = code_actions_to_value(actions);
        let res = self.with_lisp(|lisp| {
            let env = lisp.env().clone();
            rizz::runtime::apply(&f, &[actions_val], &env)
        });
        if let Err(e) = res {
            let msg = format!("lsp-code-action-fn failed: {e}");
            warn!(error = %e, "lsp code action callback failed");
            self.notify_via_lisp(&msg);
        }
    }

    /// Apply the completion at `id` from the most recent batch. Replaces
    /// any text typed between the request anchor and the current cursor
    /// with the item's `insert_text`, in the originating buffer. No-op
    /// (with notify) when the id is out of range. Called by
    /// `(lsp-apply-completion id)`.
    pub(crate) fn apply_lsp_completion_by_id(&mut self, id: usize) {
        let Some(pending) = self.lsp_pending_completion.clone() else {
            self.notify_via_lisp("lsp-apply-completion: no pending completion");
            return;
        };
        let Some(item) = pending.items.get(id) else {
            self.notify_via_lisp(&format!("lsp-apply-completion: id {id} out of range"));
            return;
        };
        let Some(b) = self.bufs.get_mut(pending.buf) else {
            return;
        };
        let rope = b.rope().clone();
        let last_line = rope.len_lines().saturating_sub(1);
        let anchor_line = rope.line_to_char(pending.anchor.row.min(last_line));
        let anchor_idx = (anchor_line + pending.anchor.col).min(rope.len_chars());
        let cursor = b.abs_pos();
        let cursor_line = rope.line_to_char(cursor.row.min(last_line));
        let cursor_idx = (cursor_line + cursor.col).min(rope.len_chars());
        let (start, end) = (anchor_idx.min(cursor_idx), anchor_idx.max(cursor_idx));
        b.land_cursor_to(pending.anchor.row, pending.anchor.col);
        if end > start {
            b.delete_range(start, end);
        }
        let text = item.insert_text.clone();
        if !text.is_empty() {
            b.insert_many(&text);
        }
    }

    /// Invoke the code action at `id` from the most recent batch — apply its
    /// workspace edit if present, then forward its command (if any) through
    /// `workspace/executeCommand`. Called by `(lsp-invoke-code-action id)`.
    pub(crate) fn invoke_lsp_code_action_by_id(&mut self, id: usize) {
        let Some(pending) = self.lsp_pending_code_actions.clone() else {
            self.notify_via_lisp("lsp-invoke-code-action: no pending code action");
            return;
        };
        let Some(action) = pending.actions.get(id).cloned() else {
            self.notify_via_lisp(&format!("lsp-invoke-code-action: id {id} out of range"));
            return;
        };
        if let Some(edit) = action.edit {
            self.apply_lsp_workspace_edit(Arc::new(edit), Arc::from(action.title.as_ref()));
        }
        if let Some(cmd) = action.command {
            let Some((client, _uri, _enc, _pos, _anchor)) = self.focused_lsp_context(pending.buf)
            else {
                self.notify_via_lisp(
                    "lsp-invoke-code-action: no language server attached to originating buffer",
                );
                return;
            };
            self.lsp_send_execute_command(client, cmd);
        }
    }

    pub(crate) fn apply_lsp_text_edits(
        &mut self,
        buf: BufferId,
        edits: Arc<[rizz_actions::TextEditOwned]>,
        _label: Arc<str>,
    ) {
        let Some(_b) = self.bufs.get_mut(buf) else {
            return;
        };
        if edits.is_empty() {
            return;
        }
        // Apply edits back-to-front so an earlier edit can't shift the
        // char positions of a later one.
        let mut sorted: Vec<&rizz_actions::TextEditOwned> = edits.iter().collect();
        sorted.sort_by(|a, b| {
            (b.range.start.row, b.range.start.col).cmp(&(a.range.start.row, a.range.start.col))
        });
        for edit in sorted {
            let b = match self.bufs.get_mut(buf) {
                Some(b) => b,
                None => return,
            };
            let rope = b.rope().clone();
            let start_line = rope.line_to_char(edit.range.start.row.min(rope.len_lines() - 1));
            let start_idx = start_line + edit.range.start.col;
            let end_line = rope.line_to_char(edit.range.end.row.min(rope.len_lines() - 1));
            let end_idx = end_line + edit.range.end.col;
            let start = start_idx.min(rope.len_chars());
            let end = end_idx.min(rope.len_chars()).max(start);
            b.land_cursor_to(edit.range.start.row, edit.range.start.col);
            if end > start {
                b.delete_range(start, end);
            }
            let s: &str = &edit.new_text;
            if !s.is_empty() {
                b.insert_many(s);
            }
        }
    }

    pub(crate) fn apply_lsp_workspace_edit(
        &mut self,
        edit: Arc<rizz_actions::WorkspaceEditOwned>,
        label: Arc<str>,
    ) {
        for doc in edit.changes.iter() {
            let Some(path) = uri_to_path(&doc.uri) else {
                continue;
            };
            let dest_buf = self.find_or_open_file(&path);
            self.apply_lsp_text_edits(dest_buf, doc.edits.clone(), label.clone());
        }
    }

    pub(crate) fn lsp_send_execute_command(
        &mut self,
        client: LspClientId,
        command: rizz_actions::CommandOwned,
    ) {
        let seq = self.next_lsp_seq();
        let arguments: Vec<serde_json::Value> = command
            .arguments_json
            .iter()
            .filter_map(|s| serde_json::from_str(s).ok())
            .collect();
        rizz_lsp::runtime().send_cmd(RuntimeCmd::ExecuteCommand {
            client,
            seq,
            command: command.command.to_string(),
            arguments,
        });
    }

    /// Drain pending LSP events and convert each to follow-up `Action`s.
    /// Called from the editor's main loop on every tick.
    pub fn drain_lsp_events(&mut self) -> Vec<Rc<Action>> {
        let mut out: Vec<Rc<Action>> = Vec::new();
        let rx = rizz_lsp::runtime().events_rx().clone();
        while let Ok(ev) = rx.try_recv() {
            self.handle_lsp_event(ev, &mut out);
        }
        out
    }

    fn handle_lsp_event(&mut self, ev: LspEvent, out: &mut Vec<Rc<Action>>) {
        match ev {
            LspEvent::Diagnostics { uri, items, .. } => {
                if let Some(&bid) = self.buf_by_uri.get(&uri)
                    && let Some(b) = self.bufs.get_mut(bid)
                    && let Some(h) = b.lsp_handle_mut()
                {
                    h.replace_diagnostics(items);
                }
            }
            LspEvent::HoverResponse { seq, contents, .. } => {
                let Some(PendingLspKind::Hover { anchor, .. }) =
                    self.pending_lsp_requests.remove(&seq)
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
                if self.pending_lsp_requests.remove(&seq).is_none() {
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
                    self.pending_lsp_requests.remove(&seq)
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
                    self.pending_lsp_requests.remove(&seq)
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
                if self.pending_lsp_requests.remove(&seq).is_none() {
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
                self.pending_lsp_requests.remove(&seq);
                warn!(seq, %message, "lsp request error");
            }
        }
    }

    /// Drain any pending LSP events and apply the synthesized actions.
    /// Call this from the main loop after every event tick (and on the
    /// `event::poll` timeout branch) so server-pushed diagnostics surface
    /// even when the user is idle.
    #[instrument(skip(self))]
    pub fn tick(&mut self) -> io::Result<bool> {
        let actions = self.drain_lsp_events();
        if actions.is_empty() {
            return Ok(false);
        }
        self.apply(&actions)?;
        Ok(true)
    }
}
