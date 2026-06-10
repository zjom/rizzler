//! Outgoing LSP requests: cursor-context resolution and the
//! `lsp_send_*_focused` senders that register a [`PendingLspKind`] and ship
//! a [`RuntimeCmd`] to the async runtime. Responses come back through the
//! drain in [`super::lsp_session`] and are displayed by
//! [`super::lsp_responses`].

use std::time::{Duration, Instant};

use rizz_actions::LspClientId;
use rizz_core::FilePos;
use rizz_lsp::{Encoding as LspEncoding, RuntimeCmd};
use rizz_text::BufferId;

use super::State;
use super::lsp_session::PendingLspKind;

pub(super) const LSP_FORMAT_TIMEOUT: Duration = Duration::from_millis(2000);

impl State {
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
        FilePos,
    )> {
        let b = self.bufs.get(buf)?;
        let handle = b.lsp_handle()?;
        // The trait-object attachment can't be downcast for client+encoding,
        // so we look it up via `buf_by_uri` (every attached buffer is
        // registered there) and the manifest entry for the file's extension.
        let uri = self.bufs.uri_for_id(buf)?;
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
        let seq = self.lsp_session.alloc_seq();
        self.lsp_session
            .pending_requests
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
        let seq = self.lsp_session.alloc_seq();
        self.lsp_session
            .pending_requests
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
        let seq = self.lsp_session.alloc_seq();
        self.lsp_session
            .pending_requests
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
        let seq = self.lsp_session.alloc_seq();
        let deadline = Instant::now() + LSP_FORMAT_TIMEOUT;
        self.lsp_session
            .pending_requests
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
        let seq = self.lsp_session.alloc_seq();
        // TODO: support visual-selection-driven ranges.
        let range = lsp_types::Range {
            start: position,
            end: position,
        };
        self.lsp_session
            .pending_requests
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
        self.bufs.unregister_uris_for(buf);
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
                .and_then(|ext| {
                    self.lang
                        .lsp
                        .manifest
                        .lookup_by_ext(&ext)
                        .map(str::to_string)
                })
        });
        let Some(server_name) = resolved_name else {
            self.notify_via_lisp(
                "lsp restart: no server name and no LSP attached to focused buffer",
            );
            return;
        };
        // Detach every buffer using this server and shut down the client;
        // the next buffer open re-spawns it.
        let uris: Vec<String> = self.bufs.uris();
        for uri in &uris {
            if let Some(bid) = self.bufs.id_for_uri(uri)
                && let Some(b) = self.bufs.get_mut(bid)
            {
                b.set_lsp_handle(None);
            }
        }
        self.bufs.clear_uris();
        self.lang.lsp_registry.shutdown(&server_name);
        self.notify_via_lisp(&format!("lsp `{server_name}` restarted"));
        self.install_lsp_client(buf);
    }

    pub(crate) fn lsp_send_execute_command(
        &mut self,
        client: LspClientId,
        command: rizz_actions::CommandOwned,
    ) {
        let seq = self.lsp_session.alloc_seq();
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
}
