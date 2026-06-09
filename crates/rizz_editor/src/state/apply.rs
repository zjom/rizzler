//! The single behaviour funnel: every key press, lisp call, or external
//! trigger ultimately produces an [`Action`] list and runs through here.
//!
//! PR4 will decompose this monolithic match into category dispatchers
//! (`apply_text` / `apply_motion` / `apply_lsp` / …). For PR1 the match stays
//! intact so the file split is pure code reorganisation.

use std::io;
use std::rc::Rc;

use rizz_actions::Action;
use rizz_core::EditingMode;
use rizz_registers::RegisterEntry;
use rizz_search::SearchDir;
use tracing::{debug, info, instrument, trace, warn};

use crate::buffer_list::CycleDir;

use super::State;

impl State {
    #[instrument(skip(self, actions), fields(count = actions.len()))]
    pub fn apply(&mut self, actions: &[Rc<Action>]) -> io::Result<()> {
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
            match action.as_ref() {
                Action::Noop => {}
                Action::Quit => {
                    info!("Action::Quit -> set quit flag");
                    self.quit = true;
                }
                Action::SetMode(m) => {
                    debug!(mode = ?m, "Action::SetMode");
                    self.set_mode(*m);
                }
                Action::InsertChar(c) => {
                    let f = self.focused_buf_id();
                    self.bufs[f].insert_char(*c);
                }
                Action::ReplaceChar(c) => {
                    let count = self.count_prefix.or_one();
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, ch = %c, count, "Action::ReplaceChar");
                    self.bufs[f].replace_char_n(*c, count);
                }
                Action::OverwriteChar(c) => {
                    let f = self.focused_buf_id();
                    trace!(buf = ?f, ch = %c, "Action::OverwriteChar");
                    self.bufs[f].overwrite_char(*c);
                }
                Action::ReplaceBackspace => {
                    let f = self.focused_buf_id();
                    trace!(buf = ?f, "Action::ReplaceBackspace");
                    self.bufs[f].replace_backspace();
                }
                Action::SpeculativeInsertChar(c) => {
                    let f = self.focused_buf_id();
                    self.bufs[f].insert_speculative_char(*c);
                }
                Action::CommitSpeculation => {
                    let f = self.focused_buf_id();
                    self.bufs[f].commit_speculation();
                }
                Action::RollbackSpeculation => {
                    let f = self.focused_buf_id();
                    self.bufs[f].rollback_speculation();
                }
                Action::InsertMany(s) => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, len = s.len(), "Action::InsertMany");
                    self.bufs[f].insert_many(s);
                }
                Action::InsertNewline => {
                    let f = self.focused_buf_id();
                    self.bufs[f].insert_char('\n');
                }
                Action::DeleteChar => {
                    let f = self.focused_buf_id();
                    self.bufs[f].delete_char();
                }
                Action::DeleteCharAt(pos) => {
                    let f = self.focused_buf_id();
                    self.bufs[f].delete_char_at(*pos);
                }
                Action::DeleteSelection => {
                    let f = self.focused_buf_id();
                    let yanked = self.bufs[f].yank_selection();
                    if self.bufs[f].delete_selection()
                        && let Some((text, kind)) = yanked
                    {
                        let name = self.pending_register.take();
                        self.registers.record_delete(text, kind, name);
                    }
                }
                Action::DeleteLine { count } => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, count, "Action::DeleteLine");
                    let yanked = self.bufs[f].yank_line(*count);
                    if self.bufs[f].delete_line(*count)
                        && let Some((text, kind)) = yanked
                    {
                        let name = self.pending_register.take();
                        self.registers.record_delete(text, kind, name);
                    }
                }
                Action::DeleteMotion { kind, count } => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, ?kind, count, "Action::DeleteMotion");
                    let yanked = self.bufs[f].yank_motion(*kind, *count);
                    if self.bufs[f].delete_motion(*kind, *count)
                        && let Some((text, kind)) = yanked
                    {
                        let name = self.pending_register.take();
                        self.registers.record_delete(text, kind, name);
                    }
                }
                Action::Undo => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, "Action::Undo");
                    self.bufs[f].undo();
                    self.bufs[f].move_cursor(rizz_text::MoveKind::Center);
                }
                Action::Redo => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, "Action::Redo");
                    self.bufs[f].redo();
                    self.bufs[f].move_cursor(rizz_text::MoveKind::Center);
                }
                Action::GotoLastEdit { count } => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, count, "Action::GotoLastEdit");
                    self.bufs[f].goto_last_edit(*count);
                    self.bufs[f].move_cursor(rizz_text::MoveKind::Center);
                }
                Action::MoveCursor { kind, count } => {
                    let f = self.focused_buf_id();
                    trace!(buf = ?f, ?kind, count, "Action::MoveCursor");
                    self.bufs[f].move_cursor_n(*kind, *count);
                }
                Action::YankMotion { kind, count } => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, ?kind, count, "Action::YankMotion");
                    if let Some((text, k)) = self.bufs[f].yank_motion(*kind, *count) {
                        let name = self.pending_register.take();
                        self.registers.record_yank(text, k, name);
                    } else {
                        self.pending_register = None;
                    }
                }
                Action::YankLine { count } => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, count, "Action::YankLine");
                    if let Some((text, k)) = self.bufs[f].yank_line(*count) {
                        let name = self.pending_register.take();
                        self.registers.record_yank(text, k, name);
                    } else {
                        self.pending_register = None;
                    }
                }
                Action::YankSelection => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, "Action::YankSelection");
                    if let Some((text, k)) = self.bufs[f].yank_selection() {
                        let name = self.pending_register.take();
                        self.registers.record_yank(text, k, name);
                    } else {
                        self.pending_register = None;
                    }
                    self.bufs[f].set_mode(EditingMode::Normal);
                }
                Action::Paste { before, count } => {
                    let name = self.pending_register.take().unwrap_or('"');
                    let entry = self.registers.read(name).cloned();
                    let Some(entry) = entry else {
                        trace!(?name, "Action::Paste: empty register");
                        continue;
                    };
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, ?name, before, count, "Action::Paste");
                    // Vim's `Np` inserts N copies of the register payload
                    // in one shot, not N successive pastes.
                    let n = (*count).max(1) as usize;
                    let entry = if n > 1 {
                        let mut joined = String::with_capacity(entry.text.len() * n);
                        for _ in 0..n {
                            joined.push_str(&entry.text);
                        }
                        RegisterEntry::new(joined, entry.kind)
                    } else {
                        entry
                    };
                    self.bufs[f].paste(&entry, *before);
                }
                Action::RegisterSelect(name) => {
                    debug!(name = ?name, "Action::RegisterSelect");
                    self.pending_register = Some(*name);
                }
                Action::RegisterSet { name, text, kind } => {
                    debug!(name = ?name, kind = ?kind, "Action::RegisterSet");
                    self.registers
                        .write(*name, RegisterEntry::new(text.clone(), *kind));
                }
                Action::YankTextObject {
                    object,
                    around,
                    count,
                } => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, ?object, around, count, "Action::YankTextObject");
                    if let Some((lo, hi, kind)) =
                        self.bufs[f].text_object_range(*object, *around, *count)
                    {
                        let text = self.bufs[f].rope().slice(lo..hi).to_string();
                        let name = self.pending_register.take();
                        self.registers.record_yank(text, kind, name);
                    } else {
                        self.pending_register = None;
                    }
                }
                Action::DeleteTextObject {
                    object,
                    around,
                    count,
                } => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, ?object, around, count, "Action::DeleteTextObject");
                    if let Some((lo, hi, kind)) =
                        self.bufs[f].text_object_range(*object, *around, *count)
                    {
                        let text = self.bufs[f].rope().slice(lo..hi).to_string();
                        if self.bufs[f].delete_range(lo, hi) {
                            let name = self.pending_register.take();
                            self.registers.record_delete(text, kind, name);
                        }
                    } else {
                        self.pending_register = None;
                    }
                }
                Action::SelectTextObject {
                    object,
                    around,
                    count,
                } => {
                    let f = self.focused_buf_id();
                    debug!(buf = ?f, ?object, around, count, "Action::SelectTextObject");
                    if let Some((lo, hi, _)) =
                        self.bufs[f].text_object_range(*object, *around, *count)
                    {
                        self.bufs[f].select_char_range(lo, hi);
                    }
                }
                Action::CommandCancel => {
                    debug!("Action::CommandCancel");
                    self.exit_minibuffer();
                }
                Action::SearchSubmit => {
                    let pattern = self.bufs.minibuffer().text();
                    debug!(pattern, "Action::SearchSubmit");
                    if pattern.is_empty() {
                        // Vim's `/<enter>` semantic: repeat last search
                        // forward from wherever live search left the cursor.
                        self.search.take_origin();
                        self.exit_minibuffer();
                        if self.search.last_pattern().is_some() {
                            rizz_search::repeat_search(self, SearchDir::Forward);
                        }
                    } else {
                        // Live search already placed cursor + overlays. Just
                        // record the pattern in `/` and drop origin so cancel
                        // can't fire later. Center the viewport on the match
                        // (vim's `nzz`, applied to submit as well as n/N).
                        self.search.take_origin();
                        self.registers.record_search(&*pattern);
                        self.exit_minibuffer();
                        let target_id = self.windows.focused_buf();
                        if let Some(b) = self.bufs.get_mut(target_id) {
                            b.move_cursor(rizz_text::MoveKind::Center);
                        }
                    }
                }
                Action::SearchCancel => {
                    debug!("Action::SearchCancel");
                    rizz_search::cancel_live_search(self);
                    self.exit_minibuffer();
                }
                Action::SearchNext => {
                    debug!("Action::SearchNext");
                    rizz_search::repeat_search(self, SearchDir::Forward);
                }
                Action::SearchPrev => {
                    debug!("Action::SearchPrev");
                    rizz_search::repeat_search(self, SearchDir::Backward);
                }
                Action::BufCreate { path, set_active } => {
                    info!(?path, set_active, "Action::BufCreate");
                    self.create_buf(*set_active, path.clone())?;
                }
                Action::BufDelete => {
                    let editor = self.windows.focused_buf();
                    info!(buf = ?editor, "Action::BufDelete");
                    self.delete_buf(editor);
                }
                Action::BufNext => {
                    debug!("Action::BufNext");
                    self.cycle_buffer(CycleDir::Next);
                }
                Action::BufPrev => {
                    debug!("Action::BufPrev");
                    self.cycle_buffer(CycleDir::Prev);
                }
                Action::BufEdit(path) => {
                    info!(?path, "Action::BufEdit");
                    self.edit_buf(path.clone())?;
                }
                Action::BufWrite(path) => {
                    info!(?path, "Action::BufWrite");
                    self.write_buf(path.clone())?;
                }
                Action::WindowSplit(dir) => {
                    info!(?dir, "Action::WindowSplit");
                    self.window_split(*dir);
                }
                Action::WindowClose => {
                    info!("Action::WindowClose");
                    self.window_close();
                }
                Action::WindowFocusNext => {
                    debug!("Action::WindowFocusNext");
                    self.windows.focus_next();
                }
                Action::WindowFocus(d) => {
                    debug!(dir = ?d, "Action::WindowFocus");
                    self.windows.focus_dir(*d);
                }
                Action::KeymapSet { mode, lhs, rhs } => {
                    debug!(%mode, keys = lhs.len(), "Action::KeymapSet");
                    self.keymap.set(mode.clone(), lhs, rhs.clone());
                }
                Action::KeymapRemove { mode, lhs } => {
                    debug!(%mode, keys = lhs.len(), "Action::KeymapRemove");
                    self.keymap.remove(mode.clone(), lhs);
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
                    self.show_lsp_hover(contents.clone(), *anchor);
                }
                Action::LspShowDefinitionList { locations } => {
                    self.show_lsp_definition_list(locations.clone());
                }
                Action::LspShowCompletion { items, anchor } => {
                    self.show_lsp_completion(items.clone(), *anchor);
                }
                Action::LspShowCodeActions { actions } => {
                    self.show_lsp_code_actions(actions.clone());
                }
                Action::LspApplyTextEdits { buf, edits, label } => {
                    self.apply_lsp_text_edits(*buf, edits.clone(), label.clone());
                }
                Action::LspApplyWorkspaceEdit { edit, label } => {
                    self.apply_lsp_workspace_edit(edit.clone(), label.clone());
                }
                Action::LspExecuteCommand { client, command } => {
                    self.lsp_send_execute_command(*client, command.clone());
                }
            }
            // After any minibuffer-text edit during a live `/` search,
            // re-anchor at origin and re-run with the new pattern. Submit/
            // Cancel/Next/Prev handle search themselves and are skipped.
            if edits_minibuffer_text && self.bufs.minibuffer().mode() == EditingMode::Search {
                rizz_search::refresh_live_search(self);
            }
        }
        Ok(())
    }
}
