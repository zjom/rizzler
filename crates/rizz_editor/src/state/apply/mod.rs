//! The single behaviour funnel: every key press, lisp call, or external
//! trigger ultimately produces an [`Action`] list and runs through here.
//!
//! [`State::apply`] is the loop plus live-search bookkeeping;
//! [`State::apply_one`] is a pure dispatch table — one match arm per
//! variant, each a single call. Bodies live in the category modules
//! ([`text`], [`registers`], [`search`]) as free functions over `State`,
//! or on the existing `impl State` subsystem files (buffers, surface,
//! lsp_session) where an arm already had a method.
//!
//! # Error policy
//!
//! Applying an action is infallible from the caller's point of view:
//! user-visible failures (a `:w` that can't write, a lisp form that
//! errors) are routed to `notify_via_lisp` + the message journal and
//! logged via `tracing` — they never abort the funnel or the editor.
//! The only fallible editor paths are terminal I/O (`State::render`)
//! and startup (`State::with_config`).

mod registers;
mod search;
mod text;

use std::rc::Rc;

use rizz_actions::Action;
use rizz_core::EditingMode;
use rizz_search::SearchDir;
use tracing::{debug, info, instrument, trace, warn};

use crate::buffer_list::CycleDir;

use super::State;

impl State {
    #[instrument(skip(self, actions), fields(count = actions.len()))]
    pub fn apply(&mut self, actions: &[Rc<Action>]) {
        for action in actions {
            trace!(action = ?action.as_ref(), "applying action");
            // Only text-mutating actions trigger a live-search refresh;
            // arrows / `<esc>` / submit handle themselves.
            let edits_minibuffer_text = matches!(
                action.as_ref(),
                Action::InsertChar(_)
                    | Action::DeleteChar
                    | Action::DeleteCharAt(_)
                    | Action::InsertMany(_)
            );
            self.apply_one(action);
            // After any minibuffer-text edit during a live `/` search,
            // re-anchor at origin and re-run with the new pattern. Submit/
            // Cancel/Next/Prev handle search themselves and are skipped.
            if edits_minibuffer_text && self.bufs.minibuffer().mode() == EditingMode::Search {
                rizz_search::refresh_live_search(self);
            }
        }
    }

    fn apply_one(&mut self, action: &Action) {
        match action {
            Action::Noop => {}
            Action::Quit => {
                info!("Action::Quit -> set quit flag");
                self.quit = true;
            }
            Action::SetMode(m) => {
                debug!(mode = ?m, "Action::SetMode");
                self.set_mode(*m);
            }

            Action::InsertChar(c) => text::insert_char(self, *c),
            Action::ReplaceChar(c) => text::replace_char(self, *c),
            Action::OverwriteChar(c) => text::overwrite_char(self, *c),
            Action::ReplaceBackspace => text::replace_backspace(self),
            Action::SpeculativeInsertChar(c) => text::speculative_insert_char(self, *c),
            Action::CommitSpeculation => text::commit_speculation(self),
            Action::RollbackSpeculation => text::rollback_speculation(self),
            Action::InsertMany(s) => text::insert_many(self, s),
            Action::InsertNewline => text::insert_newline(self),
            Action::OpenLineAbove => text::open_line_above(self),
            Action::DeleteChar => text::delete_char(self),
            Action::DeleteCharAt(pos) => text::delete_char_at(self, *pos),
            Action::ShiftLine { count, dedent } => text::shift_line(self, *count, *dedent),
            Action::ShiftSelection { dedent } => text::shift_selection(self, *dedent),
            Action::Undo => text::undo(self),
            Action::Redo => text::redo(self),
            Action::GotoLastEdit { count } => text::goto_last_edit(self, *count),
            Action::MoveCursor { kind, count } => text::move_cursor(self, *kind, *count),
            Action::SelectTextObject {
                object,
                around,
                count,
            } => text::select_text_object(self, *object, *around, *count),

            Action::DeleteSelection => registers::delete_selection(self),
            Action::DeleteLine { count } => registers::delete_line(self, *count),
            Action::DeleteMotion { kind, count } => registers::delete_motion(self, *kind, *count),
            Action::YankMotion { kind, count } => registers::yank_motion(self, *kind, *count),
            Action::YankLine { count } => registers::yank_line(self, *count),
            Action::YankSelection => registers::yank_selection(self),
            Action::Paste { before, count } => registers::paste(self, *before, *count),
            Action::RegisterSelect(name) => {
                debug!(name = ?name, "Action::RegisterSelect");
                self.pending_register = Some(*name);
            }
            Action::RegisterSet { name, text, kind } => {
                registers::register_set(self, *name, text.clone(), *kind)
            }
            Action::DeleteTextObject {
                object,
                around,
                count,
            } => registers::delete_text_object(self, *object, *around, *count),
            Action::YankTextObject {
                object,
                around,
                count,
            } => registers::yank_text_object(self, *object, *around, *count),

            Action::CommandCancel => {
                debug!("Action::CommandCancel");
                self.exit_minibuffer();
            }
            Action::SearchSubmit => search::submit(self),
            Action::SearchCancel => search::cancel(self),
            Action::SearchNext => search::repeat(self, SearchDir::Forward),
            Action::SearchPrev => search::repeat(self, SearchDir::Backward),

            Action::BufCreate { path, set_active } => {
                self.create_buf(*set_active, path.clone());
            }
            Action::BufDelete => self.delete_buf(self.surface.windows.focused_buf()),
            Action::BufNext => self.cycle_buffer(CycleDir::Next),
            Action::BufPrev => self.cycle_buffer(CycleDir::Prev),
            Action::BufEdit(path) => {
                self.edit_buf(path.clone());
            }
            Action::BufWrite(path) => self.write_buf(path.clone()),

            Action::WindowSplit(dir) => self.window_split(*dir),
            Action::WindowClose => self.window_close(),
            Action::WindowFocusNext => {
                debug!("Action::WindowFocusNext");
                self.surface.windows.focus_next();
            }
            Action::WindowFocus(d) => {
                debug!(dir = ?d, "Action::WindowFocus");
                self.surface.windows.focus_dir(*d);
            }

            Action::KeymapSet { mode, lhs, rhs } => {
                debug!(%mode, keys = lhs.len(), "Action::KeymapSet");
                self.input.keymap.set(mode.clone(), lhs, rhs.clone());
            }
            Action::KeymapRemove { mode, lhs } => {
                debug!(%mode, keys = lhs.len(), "Action::KeymapRemove");
                self.input.keymap.remove(mode.clone(), lhs);
            }
            Action::EvalLisp(form) => {
                if let Err(e) = self.eval_lisp_value(form.clone()) {
                    warn!(error = %e, "Action::EvalLisp failed -> notifying");
                    self.notify_via_lisp(&e.to_string());
                }
            }

            Action::LspHover => self.lsp_send_hover_focused(),
            Action::LspGotoDefinition => self.lsp_send_goto_definition_focused(),
            Action::LspCompletion => self.lsp_send_completion_focused(),
            Action::LspFormat => self.lsp_send_format_focused(),
            Action::LspCodeAction => self.lsp_send_code_action_focused(),
            Action::LspRestart { name } => self.lsp_restart(name.as_deref()),
            Action::LspDidOpenFocused => self.lsp_send_did_open_focused(),
            Action::LspDidCloseFocused => self.lsp_send_did_close_focused(),
            Action::LspShowHover { contents, anchor } => {
                self.show_lsp_hover(contents.clone(), *anchor)
            }
            Action::LspShowDefinitionList { locations } => {
                self.show_lsp_definition_list(locations.clone())
            }
            Action::LspShowCompletion { items, anchor } => {
                self.show_lsp_completion(items.clone(), *anchor)
            }
            Action::LspShowCodeActions { actions } => self.show_lsp_code_actions(actions.clone()),
            Action::LspApplyTextEdits { buf, edits, label } => {
                self.apply_lsp_text_edits(*buf, edits.clone(), label.clone())
            }
            Action::LspApplyWorkspaceEdit { edit, label } => {
                self.apply_lsp_workspace_edit(edit.clone(), label.clone())
            }
            Action::LspExecuteCommand { client, command } => {
                self.lsp_send_execute_command(*client, command.clone())
            }
        }
    }
}
