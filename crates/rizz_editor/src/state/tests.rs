use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use rizz::RizzError;
use rizz::runtime::Value;
use rizz_actions::Action;
use rizz_core::Position;
use rizz_registers::RegisterEntry;
use rizz_ts_install::InstallOpts;

use super::test_support::test_state;
use super::*;

#[test]
fn render_does_not_panic_on_empty_buffer() {
    let mut s = test_state();
    s.render().unwrap();
}

fn top_popup_text(s: &State) -> String {
    let id = s.top_popup_buf().expect("popup is visible");
    s.bufs[id].text()
}

fn primary(s: &State) -> BufferId {
    s.bufs.first_file_buf()
}

#[test]
fn notify_records_history_and_shows_popup() {
    let mut s = test_state();
    s.eval_lisp(r#"(notify "hello")"#).unwrap();
    assert_eq!(
        s.message_history().cloned().collect::<Vec<_>>(),
        vec!["hello".into()]
    );
    assert!(s.has_popup());
    assert_eq!(top_popup_text(&s), "hello");
}

#[test]
fn command_history_up_down_recall() {
    let mut s = test_state();
    s.record_cmd("first");
    s.record_cmd("second");
    // Entering command mode clears the minibuffer and resets recall.
    s.eval_lisp("(set-mode 'command)").unwrap();
    assert_eq!(s.minibuffer_text(), "");

    // <up> pulls in the newest entry, then walks older.
    s.command_history_prev();
    assert_eq!(s.minibuffer_text(), "second");
    s.command_history_prev();
    assert_eq!(s.minibuffer_text(), "first");
    // At the oldest entry it stays put.
    s.command_history_prev();
    assert_eq!(s.minibuffer_text(), "first");

    // <down> walks back toward newer, then restores the (empty) draft.
    s.command_history_next();
    assert_eq!(s.minibuffer_text(), "second");
    s.command_history_next();
    assert_eq!(s.minibuffer_text(), "");
    // Past the draft <down> is a no-op.
    s.command_history_next();
    assert_eq!(s.minibuffer_text(), "");
}

#[test]
fn command_history_restores_typed_draft() {
    use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
    let mut s = test_state();
    s.record_cmd("old");
    s.eval_lisp("(set-mode 'command)").unwrap();
    for c in "wip".chars() {
        s.handle_key_event(CT::new(KeyCode::Char(c), KeyModifiers::NONE))
            .unwrap();
    }
    assert_eq!(s.minibuffer_text(), "wip");

    // Recall an entry, then step back down past the newest → the draft returns.
    s.command_history_prev();
    assert_eq!(s.minibuffer_text(), "old");
    s.command_history_next();
    assert_eq!(s.minibuffer_text(), "wip");
}

#[test]
fn command_history_prev_noop_when_empty() {
    let mut s = test_state();
    s.eval_lisp("(set-mode 'command)").unwrap();
    s.command_history_prev();
    assert_eq!(s.minibuffer_text(), "");
    s.command_history_next();
    assert_eq!(s.minibuffer_text(), "");
}

#[test]
fn q_dismisses_popup() {
    use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
    let mut s = test_state();
    s.eval_lisp(r#"(notify "oops")"#).unwrap();
    assert!(s.has_popup());
    s.handle_key_event(CT::new(KeyCode::Char('q'), KeyModifiers::NONE))
        .unwrap();
    assert!(!s.has_popup());
}

#[test]
fn count_prefix_scales_motion() {
    use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear_with("a\nb\nc\nd\ne\nf\ng");
    s.handle_key_event(CT::new(KeyCode::Char('3'), KeyModifiers::NONE))
        .unwrap();
    let abs_row = s.bufs[b].cursor_pos().row as usize + s.bufs[b].file_pos().row;
    assert_eq!(abs_row, 0);
    s.handle_key_event(CT::new(KeyCode::Char('j'), KeyModifiers::NONE))
        .unwrap();
    let abs_row = s.bufs[b].cursor_pos().row as usize + s.bufs[b].file_pos().row;
    assert_eq!(abs_row, 3);
}

#[test]
fn leading_zero_falls_through_as_line_start() {
    use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear_with("hello world");
    s.handle_key_event(CT::new(KeyCode::Char('l'), KeyModifiers::NONE))
        .unwrap();
    s.handle_key_event(CT::new(KeyCode::Char('l'), KeyModifiers::NONE))
        .unwrap();
    s.handle_key_event(CT::new(KeyCode::Char('l'), KeyModifiers::NONE))
        .unwrap();
    s.handle_key_event(CT::new(KeyCode::Char('0'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(s.bufs[b].cursor_pos().col, 0);
}

#[test]
fn shift_right_chord_indents_with_count_prefix() {
    use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear_with("a\nb\nc");
    s.handle_key_event(CT::new(KeyCode::Char('2'), KeyModifiers::NONE))
        .unwrap();
    s.handle_key_event(CT::new(KeyCode::Char('>'), KeyModifiers::NONE))
        .unwrap();
    s.handle_key_event(CT::new(KeyCode::Char('>'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(s.bufs[b].text(), "    a\n    b\nc");
}

#[test]
fn visual_line_shift_left_chord_dedents_selection() {
    use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear_with("        a\n        b\nc");
    // V to select the line, j to extend over two lines, < to dedent.
    s.handle_key_event(CT::new(KeyCode::Char('V'), KeyModifiers::SHIFT))
        .unwrap();
    s.handle_key_event(CT::new(KeyCode::Char('j'), KeyModifiers::NONE))
        .unwrap();
    s.handle_key_event(CT::new(KeyCode::Char('<'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(s.bufs[b].text(), "    a\n    b\nc");
    assert_eq!(s.bufs[b].mode(), rizz_core::EditingMode::Normal);
}

#[test]
fn split_then_close_returns_to_single_window() {
    let mut s = test_state();
    s.apply(&[Rc::new(Action::WindowSplit(SplitDir::Horizontal))]);
    s.apply(&[Rc::new(Action::WindowClose)]);
    s.render().unwrap();
}

#[test]
fn quit_exits_when_last_buffer_is_clean() {
    let mut s = test_state();
    assert_eq!(s.bufs.file_buf_count(), 1);
    s.apply(&[Rc::new(Action::Quit { force: false })]);
    assert!(s.quit_requested(), "quitting the last clean buffer exits");
}

#[test]
fn quit_closes_buffer_when_more_than_one() {
    let mut s = test_state();
    s.create_buf(true, None); // now two file buffers, the new one focused
    assert_eq!(s.bufs.file_buf_count(), 2);
    s.apply(&[Rc::new(Action::Quit { force: false })]);
    assert!(!s.quit_requested(), "still other buffers open -> no exit");
    assert_eq!(
        s.bufs.file_buf_count(),
        1,
        "the focused buffer is closed, not the editor"
    );
}

#[test]
fn quit_refused_when_focused_buffer_is_modified() {
    let mut s = test_state();
    let id = s.surface.windows.focused_buf();
    s.bufs[id].insert_char('x');
    assert!(s.bufs[id].is_modified());
    s.apply(&[Rc::new(Action::Quit { force: false })]);
    assert!(!s.quit_requested(), "unsaved changes block a plain :q");
    assert!(
        s.message_history()
            .any(|m| m.contains("no write since last change")),
        "the user is told why the quit was refused"
    );
}

#[test]
fn quit_force_discards_unsaved_changes() {
    let mut s = test_state();
    let id = s.surface.windows.focused_buf();
    s.bufs[id].insert_char('x');
    s.apply(&[Rc::new(Action::Quit { force: true })]);
    assert!(s.quit_requested(), ":q! exits despite unsaved changes");
}

#[test]
fn quit_all_refused_when_any_buffer_modified() {
    let mut s = test_state();
    let first = s.surface.windows.focused_buf();
    s.create_buf(true, None); // a second, clean, focused buffer
    s.bufs[first].insert_char('x'); // dirty a *non-focused* buffer

    s.apply(&[Rc::new(Action::QuitAll { force: false })]);
    assert!(
        !s.quit_requested(),
        ":qa is refused while any buffer is unsaved"
    );

    s.apply(&[Rc::new(Action::QuitAll { force: true })]);
    assert!(s.quit_requested(), ":qa! exits despite unsaved changes");
}

#[test]
fn default_precompute_produces_expected_frame() {
    let mut s = test_state();
    let (frame, err) = s.precompute_frame();
    assert!(err.is_none(), "no frame errors expected: {err:?}");
    let id = s.surface.windows.focused_buf();
    let bf = &frame.per_buf[id];
    assert!(bf.gutter.is_some(), "expected a gutter");
    // cursor-line + selection; syntax/diagnostics need a grammar/LSP.
    assert!(bf.decorators.len() >= 2, "expected the built-in passes");
}

#[test]
fn precompute_skips_hidden_buffers() {
    let mut s = test_state();
    let shown = s.surface.windows.focused_buf();
    let hidden = s.create_buf(false, None);
    let (frame, err) = s.precompute_frame();
    assert!(err.is_none(), "no frame errors expected: {err:?}");
    assert!(frame.per_buf.contains_key(shown), "visible buffer rendered");
    assert!(
        frame.per_buf.contains_key(s.bufs.minibuffer_id()),
        "minibuffer rendered"
    );
    assert!(
        !frame.per_buf.contains_key(hidden),
        "hidden buffer must not be precomputed"
    );
}

#[test]
fn precompute_memoizes_unchanged_buffers() {
    let mut s = test_state();
    let id = s.surface.windows.focused_buf();
    s.bufs[id].clear_with("one\ntwo\nthree");

    let (frame1, _) = s.precompute_frame();
    let (frame2, _) = s.precompute_frame();
    assert!(
        Rc::ptr_eq(&frame1.per_buf[id], &frame2.per_buf[id]),
        "unchanged buffer must reuse the cached RenderedBuffer"
    );

    s.bufs[id].clear_with("one\ntwo\nthree\nfour");
    let (frame3, _) = s.precompute_frame();
    assert!(
        !Rc::ptr_eq(&frame2.per_buf[id], &frame3.per_buf[id]),
        "an edit must invalidate the cached RenderedBuffer"
    );
}

#[test]
fn register_grammar_rejects_missing_library() {
    let mut s = test_state();
    // A non-existent library path should fail the pre-flight in
    // `register_grammar` and leave the registry untouched.
    let err = s.register_grammar(
        "fake",
        &["fake".to_string()],
        Path::new("/path/does/not/exist.dylib"),
        "; empty query",
    );
    assert!(err.is_err(), "expected library load failure, got Ok");
    assert!(s.lang.ts_registry.is_empty(), "registry must stay empty");
}

#[test]
fn install_grammar_returns_helpful_error_for_unknown_name() {
    let mut s = test_state();
    let err = s
        .install_grammar("definitely-not-in-the-manifest", InstallOpts::default())
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("definitely-not-in-the-manifest"),
        "error should name the unknown grammar; got: {msg}"
    );
    assert!(s.lang.ts_registry.is_empty(), "registry must stay empty");
}

#[test]
fn grammar_installed_is_false_for_unknown() {
    let s = test_state();
    assert!(!s.grammar_installed("definitely-not-in-the-manifest"));
}

#[test]
fn notify_via_lisp_queues_when_lisp_taken() {
    // `install_highlighter`'s auto-load path runs inside lisp actions
    // (`(edit "foo.rs")` → BufCreate → install_highlighter). At that
    // point `self.lisp` has been taken by `with_lisp`, so a re-entrant
    // `eval_lisp("(notify ...)")` would panic. Instead the message must
    // queue, and then fire through the user's `(notify …)` fn when the
    // outer `with_lisp` puts the runtime back.
    let mut s = test_state();
    let lisp = s.scripting.lisp.take();
    s.notify_via_lisp("queued notification");
    assert_eq!(
        s.scripting.pending_notifications.len(),
        1,
        "expected the message to be queued, not eval'd or dropped"
    );
    s.scripting.lisp = lisp;
    s.drain_pending_notifications();
    assert!(
        s.scripting.pending_notifications.is_empty(),
        "queue must be empty after drain"
    );
    // The user's `(notify …)` runs `notify-record`, which appends to the
    // message history — so a successful drain leaves the message there.
    let found = s
        .message_history()
        .any(|m| m.as_ref() == "queued notification");
    assert!(
        found,
        "drain should have routed the message through `(notify …)`"
    );
}

#[test]
fn with_lisp_drains_queued_notifications() {
    // End-to-end: anything that queues during the body of a `with_lisp`
    // call fires through `(notify …)` on the way out.
    let mut s = test_state();
    let r: Result<_, RizzError> = s.with_lisp(|_| {
        // Inside the closure `self.lisp` is None — simulate the auto-load
        // path by reaching back through the editor bridge.
        crate::lisp::with_editor_mut(|st| st.notify_via_lisp("drained via with_lisp"));
        Ok(())
    });
    r.unwrap();
    assert!(s.scripting.pending_notifications.is_empty());
    let found = s
        .message_history()
        .any(|m| m.as_ref() == "drained via with_lisp");
    assert!(found, "with_lisp must drain queued notifications on exit");
}

/// End-to-end: a lisp callback installed via `set-lsp-completion-fn`
/// runs when `show_lsp_completion` fires, receives the items as a
/// structured array (not a flattened notify string), and an item's
/// `insert-text` can be applied via `lsp-apply-completion`.
#[test]
fn lsp_completion_callback_receives_items_and_applies() {
    use rizz_actions::{CompletionItemKindOwned, CompletionItemOwned};
    let mut s = test_state();
    // Capture the items the callback was handed into a ref so we can
    // assert on the structure from outside lisp.
    s.eval_lisp("(let _lsp-comp-seen (ref ()))").unwrap();
    s.eval_lisp("(fn _lsp-comp (items anchor) (set! _lsp-comp-seen items))")
        .unwrap();
    s.eval_lisp("(set-lsp-completion-fn _lsp-comp)").unwrap();

    let items: Arc<[CompletionItemOwned]> = Arc::from(vec![
        CompletionItemOwned {
            label: Arc::from("println!"),
            detail: None,
            insert_text: Arc::from("println!"),
            kind: CompletionItemKindOwned::Function,
        },
        CompletionItemOwned {
            label: Arc::from("print!"),
            detail: Some(Arc::from("macro")),
            insert_text: Arc::from("print!"),
            kind: CompletionItemKindOwned::Function,
        },
    ]);
    let anchor = Position { row: 0, col: 0 };
    s.show_lsp_completion(items, anchor);

    // The callback should have seen an array of two maps.
    let seen_label = s
        .eval_lisp("(get (get (deref _lsp-comp-seen) 0) \"label\")")
        .unwrap();
    assert_eq!(seen_label.as_str().as_deref(), Some("println!"));
    let seen_kind = s
        .eval_lisp("(get (get (deref _lsp-comp-seen) 0) \"kind\")")
        .unwrap();
    match &*seen_kind {
        Value::Ident(s) => assert_eq!(s.as_ref(), "function"),
        other => panic!("expected ident kind, got {other:?}"),
    }

    // Applying id=1 should insert `print!` at the originating buffer.
    s.set_mode(EditingMode::Insert);
    s.eval_lisp("(lsp-apply-completion 1)").unwrap();
    let text = s.focused_buf().text();
    assert!(text.starts_with("print!"), "got buffer text: {text:?}");
}

/// End-to-end: a lisp callback installed via `set-lsp-code-action-fn`
/// receives the actions as a structured array. Out-of-range id falls
/// through as a notify rather than panicking.
#[test]
fn lsp_code_action_callback_receives_actions_and_handles_bad_id() {
    use rizz_actions::CodeActionOwned;
    let mut s = test_state();
    s.eval_lisp("(let _lsp-ca-seen (ref ()))").unwrap();
    s.eval_lisp("(fn _lsp-ca (actions) (set! _lsp-ca-seen actions))")
        .unwrap();
    s.eval_lisp("(set-lsp-code-action-fn _lsp-ca)").unwrap();

    let actions: Arc<[CodeActionOwned]> = Arc::from(vec![CodeActionOwned {
        title: Arc::from("Import `HashMap`"),
        kind: Some(Arc::from("quickfix")),
        edit: None,
        command: None,
    }]);
    s.show_lsp_code_actions(actions);

    let seen_title = s
        .eval_lisp("(get (get (deref _lsp-ca-seen) 0) \"title\")")
        .unwrap();
    assert_eq!(seen_title.as_str().as_deref(), Some("Import `HashMap`"));

    // Out-of-range id is a graceful no-op (notify path).
    s.eval_lisp("(lsp-invoke-code-action 42)").unwrap();
}

/// Clearing the callback with `()` reverts to the notify-string
/// fallback the older code shipped. init.rz installs a popup-based
/// callback by default, so this test clears it first.
#[test]
fn lsp_completion_with_no_callback_falls_back_to_notify() {
    use rizz_actions::{CompletionItemKindOwned, CompletionItemOwned};
    let mut s = test_state();
    s.eval_lisp("(set-lsp-completion-fn ())").unwrap();
    let items: Arc<[CompletionItemOwned]> = Arc::from(vec![CompletionItemOwned {
        label: Arc::from("only"),
        detail: None,
        insert_text: Arc::from("only"),
        kind: CompletionItemKindOwned::Text,
    }]);
    s.show_lsp_completion(items, Position { row: 0, col: 0 });
    let found = s.message_history().any(|m| m.as_ref().contains("only"));
    assert!(found, "notify fallback should still surface the label");
}

#[test]
fn can_insert_j() {
    use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear();
    s.set_mode(EditingMode::Insert);
    s.handle_key_event(CT::new(KeyCode::Char('j'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(s.bufs[b].text(), "j".to_string())
}

#[test]
fn jk_chord_rolls_back_speculative_j() {
    // Typing the full `jk` escape chord must leave the buffer empty
    // (speculation rolled back) and switch to normal mode.
    use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear();
    s.set_mode(EditingMode::Insert);
    s.handle_key_event(CT::new(KeyCode::Char('j'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(s.bufs[b].text(), "j".to_string());
    s.handle_key_event(CT::new(KeyCode::Char('k'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(s.bufs[b].text(), "".to_string());
    assert_eq!(s.bufs[b].mode(), EditingMode::Normal);
}

#[test]
fn aborted_jk_chord_commits_speculative_j() {
    // Typing `j` then a non-`k` key commits the speculation and inserts
    // the new key. The two should end up as one undo step.
    use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear();
    s.set_mode(EditingMode::Insert);
    s.handle_key_event(CT::new(KeyCode::Char('j'), KeyModifiers::NONE))
        .unwrap();
    s.handle_key_event(CT::new(KeyCode::Char('x'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(s.bufs[b].text(), "jx".to_string());
    // Both chars belong to the same insert run.
    s.bufs[b].undo();
    assert_eq!(s.bufs[b].text(), "".to_string());
}

fn reg_text(s: &State, name: char) -> Option<String> {
    s.registers().read(name).map(|e| e.text.to_string())
}

#[test]
fn yank_line_fills_unnamed_and_zero() {
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear_with("hello\nworld\n");
    s.apply(&[Rc::new(Action::YankLine { count: 1 })]);
    assert_eq!(reg_text(&s, '"').as_deref(), Some("hello\n"));
    assert_eq!(reg_text(&s, '0').as_deref(), Some("hello\n"));
    // numbered 1-9 stay untouched on yank
    assert!(s.registers().read('1').is_none());
}

#[test]
fn delete_line_rotates_numbered_register() {
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear_with("a\nb\nc\nd\n");
    s.apply(&[Rc::new(Action::DeleteLine { count: 1 })]);
    assert_eq!(reg_text(&s, '1').as_deref(), Some("a\n"));
    s.apply(&[Rc::new(Action::DeleteLine { count: 1 })]);
    assert_eq!(reg_text(&s, '1').as_deref(), Some("b\n"));
    assert_eq!(reg_text(&s, '2').as_deref(), Some("a\n"));
    // delete never fills the yank register
    assert!(s.registers().read('0').is_none());
}

#[test]
fn yank_then_paste_after_inserts_below() {
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear_with("hello\nworld\n");
    s.apply(&[Rc::new(Action::YankLine { count: 1 })]);
    // move to second line
    s.bufs[b].move_cursor_n(rizz_text::MoveKind::Relative(Position::new(0, 1)), 1);
    s.apply(&[Rc::new(Action::Paste {
        before: false,
        count: 1,
    })]);
    assert_eq!(s.bufs[b].text(), "hello\nworld\nhello\n");
}

#[test]
fn paste_count_repeats_entry() {
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear_with("abc");
    // seed the unnamed register without going through delete/yank
    s.registers_mut().write('"', RegisterEntry::charwise("X"));
    s.apply(&[Rc::new(Action::Paste {
        before: false,
        count: 3,
    })]);
    assert_eq!(s.bufs[b].text(), "aXXXbc");
}

#[test]
fn register_select_targets_named_register() {
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear_with("alpha\nbeta\n");
    s.apply(&[
        Rc::new(Action::RegisterSelect('a')),
        Rc::new(Action::YankLine { count: 1 }),
    ]);
    assert_eq!(reg_text(&s, 'a').as_deref(), Some("alpha\n"));
    // pending register is cleared after a consuming action
    assert!(s.pending_register().is_none());
    // and the unnamed register also got the same text
    assert_eq!(reg_text(&s, '"').as_deref(), Some("alpha\n"));
}

#[test]
fn paste_from_named_register() {
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear_with("abc");
    s.registers_mut().write('a', RegisterEntry::charwise("ZZ"));
    s.apply(&[
        Rc::new(Action::RegisterSelect('a')),
        Rc::new(Action::Paste {
            before: false,
            count: 1,
        }),
    ]);
    assert_eq!(s.bufs[b].text(), "aZZbc");
}

#[test]
fn delete_selection_fills_unnamed() {
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear_with("hello");
    s.bufs[b].set_mode(EditingMode::Visual);
    s.bufs[b].move_cursor_n(rizz_text::MoveKind::Relative(Position::new(2, 0)), 1);
    s.apply(&[Rc::new(Action::DeleteSelection)]);
    assert_eq!(s.bufs[b].text(), "lo");
    assert_eq!(reg_text(&s, '"').as_deref(), Some("hel"));
    assert_eq!(reg_text(&s, '-').as_deref(), Some("hel"));
}

#[test]
fn yank_selection_returns_to_normal() {
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear_with("hello");
    s.bufs[b].set_mode(EditingMode::Visual);
    s.bufs[b].move_cursor_n(rizz_text::MoveKind::Relative(Position::new(2, 0)), 1);
    s.apply(&[Rc::new(Action::YankSelection)]);
    assert_eq!(reg_text(&s, '"').as_deref(), Some("hel"));
    assert_eq!(s.bufs[b].mode(), EditingMode::Normal);
    // buffer text is unchanged by yank
    assert_eq!(s.bufs[b].text(), "hello");
}

#[test]
fn lisp_register_read_and_write_round_trip() {
    let mut s = test_state();
    s.eval_lisp(r#"(register-write "a" "hello")"#).unwrap();
    let v = s.eval_lisp(r#"(register-read "a")"#).unwrap();
    assert_eq!(v.display().to_string(), "hello");
}

#[test]
fn lisp_yank_then_paste_charwise() {
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear_with("hello world");
    s.eval_lisp("(yank-motion 'word-forward)").unwrap();
    assert_eq!(reg_text(&s, '"').as_deref(), Some("hello "));
    // paste-before so the inserted text lands at the cursor (col 0)
    s.eval_lisp("(paste-before)").unwrap();
    assert_eq!(s.bufs[b].text(), "hello hello world");
}

#[test]
fn delete_inner_word_under_cursor() {
    use rizz_text::TextObject;
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear_with("hello world");
    s.bufs[b].move_cursor_n(rizz_text::MoveKind::Relative(Position::new(2, 0)), 1);
    s.apply(&[Rc::new(Action::DeleteTextObject {
        object: TextObject::Word,
        around: false,
        count: 1,
    })]);
    assert_eq!(s.bufs[b].text(), " world");
    assert_eq!(reg_text(&s, '"').as_deref(), Some("hello"));
}

#[test]
fn yank_around_paren_block_includes_brackets() {
    use rizz_text::TextObject;
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear_with("foo(bar)baz");
    s.bufs[b].move_cursor_n(rizz_text::MoveKind::Relative(Position::new(5, 0)), 1);
    s.apply(&[Rc::new(Action::YankTextObject {
        object: TextObject::Paren,
        around: true,
        count: 1,
    })]);
    assert_eq!(reg_text(&s, '"').as_deref(), Some("(bar)"));
    // buffer text is unchanged
    assert_eq!(s.bufs[b].text(), "foo(bar)baz");
}

#[test]
fn select_inner_dquote_drops_into_visual() {
    use rizz_text::TextObject;
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear_with(r#"x "hello" y"#);
    s.bufs[b].move_cursor_n(rizz_text::MoveKind::Relative(Position::new(5, 0)), 1);
    s.apply(&[Rc::new(Action::SelectTextObject {
        object: TextObject::DoubleQuote,
        around: false,
        count: 1,
    })]);
    assert_eq!(s.bufs[b].mode(), EditingMode::Visual);
    assert_eq!(s.bufs[b].selected_text().as_deref(), Some("hello"));
}

#[test]
fn lisp_delete_inner_word_works() {
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear_with("hello world");
    s.bufs[b].move_cursor_n(rizz_text::MoveKind::Relative(Position::new(2, 0)), 1);
    s.eval_lisp(r#"(delete-inner "word")"#).unwrap();
    assert_eq!(s.bufs[b].text(), " world");
}

#[test]
fn lisp_yank_around_paren_works() {
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear_with("foo(bar)");
    s.bufs[b].move_cursor_n(rizz_text::MoveKind::Relative(Position::new(5, 0)), 1);
    s.eval_lisp(r#"(yank-around "paren")"#).unwrap();
    assert_eq!(reg_text(&s, '"').as_deref(), Some("(bar)"));
}

#[test]
fn lisp_select_inner_paren_visual_mode() {
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear_with("foo(bar)");
    s.bufs[b].move_cursor_n(rizz_text::MoveKind::Relative(Position::new(5, 0)), 1);
    s.eval_lisp(r#"(select-inner "paren")"#).unwrap();
    assert_eq!(s.bufs[b].selected_text().as_deref(), Some("bar"));
}

#[test]
fn r_chord_replaces_char_under_cursor() {
    use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear_with("hello");
    s.handle_key_event(CT::new(KeyCode::Char('r'), KeyModifiers::NONE))
        .unwrap();
    // Chord descended; nothing changed yet.
    assert_eq!(s.bufs[b].text(), "hello");
    s.handle_key_event(CT::new(KeyCode::Char('X'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(s.bufs[b].text(), "Xello");
    assert_eq!(s.bufs[b].mode(), EditingMode::Normal);
}

#[test]
fn count_prefix_scales_r_chord() {
    use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear_with("hello");
    s.handle_key_event(CT::new(KeyCode::Char('3'), KeyModifiers::NONE))
        .unwrap();
    s.handle_key_event(CT::new(KeyCode::Char('r'), KeyModifiers::NONE))
        .unwrap();
    s.handle_key_event(CT::new(KeyCode::Char('z'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(s.bufs[b].text(), "zzzlo");
}

#[test]
fn capital_r_enters_replace_mode_and_overwrites() {
    use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear_with("hello");
    s.handle_key_event(CT::new(KeyCode::Char('R'), KeyModifiers::SHIFT))
        .unwrap();
    assert_eq!(s.bufs[b].mode(), EditingMode::Replace);
    s.handle_key_event(CT::new(KeyCode::Char('H'), KeyModifiers::SHIFT))
        .unwrap();
    s.handle_key_event(CT::new(KeyCode::Char('I'), KeyModifiers::SHIFT))
        .unwrap();
    assert_eq!(s.bufs[b].text(), "HIllo");
    s.handle_key_event(CT::new(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(s.bufs[b].mode(), EditingMode::Normal);
}

#[test]
fn replace_mode_at_eol_extends_line() {
    use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear_with("hi");
    // Park the cursor past the last char (`A` semantics) before
    // entering Replace mode: subsequent overwrites have to fall back
    // to insert because there's no char under the cursor to replace.
    s.bufs[b].set_mode(EditingMode::Insert);
    s.bufs[b].move_cursor_n(rizz_text::MoveKind::LineEnd, 1);
    s.bufs[b].set_mode(EditingMode::Replace);
    s.handle_key_event(CT::new(KeyCode::Char('!'), KeyModifiers::NONE))
        .unwrap();
    s.handle_key_event(CT::new(KeyCode::Char('?'), KeyModifiers::NONE))
        .unwrap();
    assert_eq!(s.bufs[b].text(), "hi!?");
}

#[test]
fn replace_mode_backspace_restores_original_chars() {
    use crossterm::event::{KeyCode, KeyEvent as CT, KeyModifiers};
    let mut s = test_state();
    let b = primary(&s);
    s.bufs[b].clear_with("hello");
    s.handle_key_event(CT::new(KeyCode::Char('R'), KeyModifiers::SHIFT))
        .unwrap();
    s.handle_key_event(CT::new(KeyCode::Char('H'), KeyModifiers::SHIFT))
        .unwrap();
    s.handle_key_event(CT::new(KeyCode::Char('I'), KeyModifiers::SHIFT))
        .unwrap();
    assert_eq!(s.bufs[b].text(), "HIllo");
    s.handle_key_event(CT::new(KeyCode::Backspace, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(s.bufs[b].text(), "Hello");
    s.handle_key_event(CT::new(KeyCode::Backspace, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(s.bufs[b].text(), "hello");
    // Past the session start: keymap-level no-op (cursor doesn't move,
    // buffer doesn't change).
    s.handle_key_event(CT::new(KeyCode::Backspace, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(s.bufs[b].text(), "hello");
    s.handle_key_event(CT::new(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    // Nothing was committed — there's no edit to undo.
    assert!(!s.bufs[b].undo());
}

// ---------------------------------------------------------------------------
// LSP event drain: synthetic `LspEvent`s routed through `handle_lsp_event`
// exactly as `tick` does — no live server or async runtime needed.
// ---------------------------------------------------------------------------

mod lsp_drain {
    use super::super::lsp_session::PendingLspKind;
    use super::super::test_support::{test_state, test_state_with_text};
    use super::*;
    use rizz_actions::{LspClientId, RangeOwned, TextEditOwned};
    use rizz_lsp::LspEvent;
    use std::time::{Duration, Instant};

    fn drain_one(s: &mut State, ev: LspEvent) -> Vec<Rc<Action>> {
        let mut out = Vec::new();
        s.handle_lsp_event(ev, &mut out);
        out
    }

    #[test]
    fn hover_response_routes_via_pending_and_notifies() {
        let mut s = test_state();
        let b = s.bufs.first_file_buf();
        let seq = s.lsp_session.alloc_seq();
        s.lsp_session.pending_requests.insert(
            seq,
            PendingLspKind::Hover {
                buf: b,
                anchor: Position { row: 0, col: 0 },
            },
        );
        let out = drain_one(
            &mut s,
            LspEvent::HoverResponse {
                client: LspClientId(7),
                seq,
                contents: Some("Type: Foo".into()),
            },
        );
        assert_eq!(out.len(), 1, "hover response must yield one show action");
        s.apply(&out);
        assert!(
            s.message_history().any(|m| m.contains("hover: Type: Foo")),
            "hover contents must reach the user"
        );
        assert!(s.lsp_session.pending_requests.is_empty());
    }

    #[test]
    fn response_with_unknown_seq_is_dropped() {
        let mut s = test_state();
        let out = drain_one(
            &mut s,
            LspEvent::HoverResponse {
                client: LspClientId(7),
                seq: 999,
                contents: Some("stale".into()),
            },
        );
        assert!(out.is_empty(), "stale responses must not produce actions");
    }

    #[test]
    fn format_response_applies_edits_to_buffer() {
        let mut s = test_state_with_text("hello world");
        let b = s.bufs.first_file_buf();
        let seq = s.lsp_session.alloc_seq();
        s.lsp_session.pending_requests.insert(
            seq,
            PendingLspKind::Format {
                buf: b,
                deadline: Instant::now() + Duration::from_secs(60),
            },
        );
        let edits = vec![TextEditOwned {
            range: RangeOwned {
                start: Position { row: 0, col: 0 },
                end: Position { row: 0, col: 5 },
            },
            new_text: Arc::from("goodbye"),
        }];
        let out = drain_one(
            &mut s,
            LspEvent::FormattingResponse {
                client: LspClientId(7),
                seq,
                edits,
            },
        );
        assert_eq!(out.len(), 1);
        s.apply(&out);
        assert_eq!(s.bufs[b].text(), "goodbye world");
    }

    #[test]
    fn format_response_after_deadline_is_dropped_with_notice() {
        let mut s = test_state_with_text("untouched");
        let b = s.bufs.first_file_buf();
        let seq = s.lsp_session.alloc_seq();
        s.lsp_session.pending_requests.insert(
            seq,
            PendingLspKind::Format {
                buf: b,
                deadline: Instant::now() - Duration::from_millis(1),
            },
        );
        let out = drain_one(
            &mut s,
            LspEvent::FormattingResponse {
                client: LspClientId(7),
                seq,
                edits: vec![],
            },
        );
        assert!(out.is_empty());
        assert!(s.message_history().any(|m| m.contains("timed out")));
        assert_eq!(s.bufs[b].text(), "untouched");
    }

    #[test]
    fn empty_definition_response_notifies_no_definition() {
        let mut s = test_state();
        let b = s.bufs.first_file_buf();
        let seq = s.lsp_session.alloc_seq();
        s.lsp_session
            .pending_requests
            .insert(seq, PendingLspKind::GotoDefinition { buf: b });
        let out = drain_one(
            &mut s,
            LspEvent::DefinitionResponse {
                client: LspClientId(7),
                seq,
                locations: vec![],
            },
        );
        s.apply(&out);
        assert!(
            s.message_history()
                .any(|m| m.contains("no definition found"))
        );
    }

    #[test]
    fn request_error_clears_pending_request() {
        let mut s = test_state();
        let b = s.bufs.first_file_buf();
        let seq = s.lsp_session.alloc_seq();
        s.lsp_session
            .pending_requests
            .insert(seq, PendingLspKind::GotoDefinition { buf: b });
        let out = drain_one(
            &mut s,
            LspEvent::RequestError {
                client: LspClientId(7),
                seq,
                message: "server fell over".into(),
            },
        );
        assert!(out.is_empty());
        assert!(
            s.lsp_session.pending_requests.is_empty(),
            "errored requests must not leak in the pending map"
        );
    }
}
