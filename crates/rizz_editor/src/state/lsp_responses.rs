//! LSP response display + application: the `show_lsp_*` surfaces (notify
//! fallback or lisp callback), the `(lsp-apply-completion id)` /
//! `(lsp-invoke-code-action id)` side-table consumers, and text/workspace
//! edit application. Requests originate in [`super::lsp_requests`]; the
//! shared bookkeeping lives in [`super::lsp_session`].

use std::sync::Arc;

use rizz_core::Position;
use rizz_text::BufferId;
use tracing::warn;

use super::lsp_session::{PendingCodeActions, PendingCompletion};
use super::{State, workspace::uri_to_path};

impl State {
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
            self.lsp_session.pending_completion = None;
            // Hand `[]` to the callback so the lisp side can dismiss any
            // open completion popup.
            if self.lsp_session.completion_fn.is_some() {
                self.fire_lsp_completion_fn(&items, anchor);
            } else {
                self.notify_via_lisp("no completions");
            }
            return;
        }
        let buf = self.focused_buf_id();
        self.lsp_session.pending_completion = Some(PendingCompletion {
            buf,
            anchor,
            items: items.clone(),
        });
        if self.lsp_session.completion_fn.is_some() {
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
        let Some(f) = self.lsp_session.completion_fn.clone() else {
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
            self.lsp_session.pending_code_actions = None;
            if self.lsp_session.code_action_fn.is_some() {
                self.fire_lsp_code_action_fn(&actions);
            } else {
                self.notify_via_lisp("no code actions");
            }
            return;
        }
        let buf = self.focused_buf_id();
        self.lsp_session.pending_code_actions = Some(PendingCodeActions {
            buf,
            actions: actions.clone(),
        });
        if self.lsp_session.code_action_fn.is_some() {
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
        let Some(f) = self.lsp_session.code_action_fn.clone() else {
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
        let Some(pending) = self.lsp_session.pending_completion.clone() else {
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
        let Some(pending) = self.lsp_session.pending_code_actions.clone() else {
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
}
