//! Embedded lisp runtime (rizz) bridged to the editor.
//!
//! The runtime owns a persistent [`Env`] of bindings — including every editor
//! primitive exposed as a native function — and is threaded through one entry
//! point on `State` (`State::eval_lisp` / `eval_lisp_value`). Editor primitives
//! access mutable `State` via an RAII-guarded thread-local pointer (see
//! `with_editor_mut`): the runtime is moved out of `State` for the duration
//! of an eval, so there is never simultaneous aliasing of `&mut State`.
//!
//! Module layout:
//! - [`helpers`] — the [`Builtins`](helpers::Builtins) registration sink plus
//!   shared value-conversion and mode-name parsers used by every builtin.
//! - [`popup_parse`] — translates the `(popup-show ...)` options map into
//!   the editor's [`PopupSpec`](crate::state::PopupSpec).
//! - [`builtins`] — per-domain registration modules (text, motion, bufs,
//!   windows, keymap, minibuffer, popups, queries, wrap, styling, widgets,
//!   textprops, misc, fs, grammar). Each contributes its slice of native
//!   functions to the shared `Builtins`.

use std::cell::Cell;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::rc::Rc;

use rizz::runtime::{Env, Value};
use rizz::{RizzError, Runtime};
use tracing::{debug, instrument, trace};

use crate::state::State;

mod builtins;
mod helpers;
mod popup_parse;

pub use helpers::quote_for_lisp;

// ---------------------------------------------------------------------------
// Editor bridge: thread-local pointer to the live `State`.
// ---------------------------------------------------------------------------

thread_local! {
    static EDITOR: Cell<Option<NonNull<State>>> = const { Cell::new(None) };
    /// True while `State::precompute_frame` is walking the slot registry.
    /// Lisp builtins that would mutate buffer state error out, so a render
    /// callback can't corrupt the in-flight frame.
    static RENDER_PHASE: Cell<bool> = const { Cell::new(false) };
}

/// RAII guard that flips `RENDER_PHASE` to true for the duration of the
/// precompute pass.
pub struct RenderPhaseGuard;

impl RenderPhaseGuard {
    pub fn enter() -> Self {
        RENDER_PHASE.with(|c| c.set(true));
        Self
    }
}

impl Drop for RenderPhaseGuard {
    fn drop(&mut self) {
        RENDER_PHASE.with(|c| c.set(false));
    }
}

pub(crate) fn in_render_phase() -> bool {
    RENDER_PHASE.with(|c| c.get())
}

/// RAII guard: stashes `state` in the thread-local for the lifetime of the
/// guard, restoring the previous value on drop.
pub struct EditorGuard {
    prev: Option<NonNull<State>>,
}

impl EditorGuard {
    pub fn new(state: &mut State) -> Self {
        let prev = EDITOR.with(|c| c.replace(Some(NonNull::from(state))));
        Self { prev }
    }
}

impl Drop for EditorGuard {
    fn drop(&mut self) {
        EDITOR.with(|c| c.set(self.prev));
    }
}

/// Run `f` with mutable access to the live `State`. Panics if called outside
/// the dynamic scope of an `EditorGuard`.
pub(crate) fn with_editor_mut<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let ptr = EDITOR
        .with(|c| c.get())
        .expect("editor bridge not active: lisp builtin called outside eval_lisp");
    // SAFETY: `EditorGuard` is alive iff some `State::eval_lisp*` is on the
    // stack with unique `&mut self`. That call holds `self` exclusively (the
    // runtime is taken out of `self.lisp` for the duration), so dereferencing
    // the pointer cannot alias any other live borrow.
    unsafe { f(ptr.as_ptr().as_mut().unwrap()) }
}

// ---------------------------------------------------------------------------
// LispRuntime
// ---------------------------------------------------------------------------

pub struct LispRuntime(Runtime);

impl LispRuntime {
    pub fn new() -> Self {
        let env = editor_env().union(rizz::prelude::env());
        Self::with_env(env)
    }
    pub fn with_env(env: Env) -> Self {
        Self(Runtime::with_env(env))
    }

    pub fn set_basedir(&mut self, p: impl Into<PathBuf>) {
        let p = p.into();
        debug!(basedir = %p.display(), "lisp runtime basedir set");
        self.0 = Runtime::with_env(self.0.env().clone().with_base_dir(Some(p)))
    }

    #[instrument(skip(self, src), fields(bytes = src.len()))]
    pub fn eval_str(&mut self, src: &str) -> Result<Rc<Value>, RizzError> {
        self.0.eval(src.as_bytes())
    }

    #[instrument(skip(self, form))]
    pub fn eval_value(&mut self, form: Rc<Value>) -> Result<Rc<Value>, RizzError> {
        trace!(form = %form.display(), "eval_form");
        Ok(self.0.eval_form(form)?)
    }

    #[instrument(skip(self, src), fields(bytes = src.len()))]
    pub fn eval_script(&mut self, src: &str) -> Result<(), RizzError> {
        self.eval_str(src)?;
        Ok(())
    }

    /// Borrow the current environment.
    pub fn env(&self) -> &Env {
        self.0.env()
    }
}

impl Default for LispRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the editor-flavoured `Env`: every native primitive registered by the
/// per-domain modules under [`builtins`], with aliases resolved.
fn editor_env() -> Env {
    let mut b = helpers::Builtins::new();
    builtins::register_all(&mut b);
    b.build()
}

#[cfg(test)]
mod tests {
    use super::helpers::wrap_shell_style;
    use super::*;
    use crate::state::test_support::test_state;

    #[test]
    fn arithmetic_from_prelude_works() {
        let mut s = test_state();
        let v = s.eval_lisp("(+ 1 2)").unwrap();
        assert_eq!(*v, Value::Int(3));
    }

    #[test]
    fn insert_char_from_lisp_mutates_buffer() {
        let mut s = test_state();
        s.eval_lisp("(insert-char \"a\")").unwrap();
        assert!(s.focused_buf().text().starts_with('a'));
    }

    #[test]
    fn wrap_shell_style_translates_input() {
        assert_eq!(wrap_shell_style("quit"), "(quit)");
        assert_eq!(wrap_shell_style("edit foo.txt"), "(edit foo.txt)");
        assert_eq!(wrap_shell_style("(+ 1 2)"), "(+ 1 2)");
        assert_eq!(wrap_shell_style("+ 1 2"), "(+ 1 2)");
        assert_eq!(wrap_shell_style("42"), "(line 42)");
        assert_eq!(wrap_shell_style("   "), "()");
    }

    #[test]
    fn keymap_set_from_lisp_binds_key() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut s = test_state();
        s.eval_lisp("(keymap-set 'normal \"q\" '(quit))").unwrap();
        s.handle_key_event(crossterm::event::KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
        ))
        .unwrap();
        assert!(s.quit_requested());
    }

    #[test]
    fn keymap_set_from_lisp_binds_modified_key() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut s = test_state();
        s.eval_lisp(r#"(keymap-set 'normal "<c-w>q" '(quit))"#)
            .unwrap();
        s.handle_key_event(crossterm::event::KeyEvent::new(
            KeyCode::Char('w'),
            KeyModifiers::CONTROL,
        ))
        .unwrap();
        s.handle_key_event(crossterm::event::KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
        ))
        .unwrap();
        assert!(s.quit_requested());
    }

    #[test]
    fn command_completions_filters_by_prefix() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut s = test_state();
        // Enter command mode and type "ed" — should match "edit"/"evaluate".
        for (code, mods) in [
            (KeyCode::Char(':'), KeyModifiers::NONE),
            (KeyCode::Char('e'), KeyModifiers::NONE),
            (KeyCode::Char('d'), KeyModifiers::NONE),
        ] {
            s.handle_key_event(crossterm::event::KeyEvent::new(code, mods))
                .unwrap();
        }
        let v = s.eval_lisp("(command-completions)").unwrap();
        let arr = v.as_array().expect("array");
        let names: Vec<String> = arr.iter().map(|x| x.display()).collect();
        assert!(
            names.contains(&"edit".to_string()),
            "missing edit: {names:?}"
        );
        assert!(names.iter().all(|n| n.starts_with("ed")));
        // private (_-prefixed) bindings should be filtered out.
        assert!(names.iter().all(|n| !n.starts_with('_')));
    }

    #[test]
    fn command_complete_replaces_token_at_cursor() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut s = test_state();
        for (code, mods) in [
            (KeyCode::Char(':'), KeyModifiers::NONE),
            (KeyCode::Char('q'), KeyModifiers::NONE),
        ] {
            s.handle_key_event(crossterm::event::KeyEvent::new(code, mods))
                .unwrap();
        }
        s.eval_lisp(r#"(command-complete "quit")"#).unwrap();
        let v = s.eval_lisp("(command-prefix)").unwrap();
        assert_eq!(v.display(), "quit");
    }

    /// Multi-match path: `:bufn<tab>` should advance to `buf-n` (the
    /// longest prefix shared by `buf-next` and the rest of the `buf-n*`
    /// family) without picking any one — leaving the user room to narrow.
    #[test]
    fn tab_in_command_mode_advances_to_longest_common_prefix() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut s = test_state();
        for (code, mods) in [
            (KeyCode::Char(':'), KeyModifiers::NONE),
            (KeyCode::Char('b'), KeyModifiers::NONE),
            (KeyCode::Char('u'), KeyModifiers::NONE),
            (KeyCode::Char('f'), KeyModifiers::NONE),
            (KeyCode::Char('-'), KeyModifiers::NONE),
            (KeyCode::Char('n'), KeyModifiers::NONE),
            (KeyCode::Tab, KeyModifiers::NONE),
        ] {
            s.handle_key_event(crossterm::event::KeyEvent::new(code, mods))
                .unwrap();
        }
        let v = s.eval_lisp("(command-prefix)").unwrap();
        // `buf-no` and `buf-next` share `buf-n`; tab should land there.
        assert_eq!(v.display(), "buf-n");
    }

    /// End-to-end: `:q<tab>` should complete to `quit` once init.rz has
    /// bound `<tab>` in command mode. Exercises the path from key event →
    /// keymap → `_command-tab` fn → `command-complete` builtin.
    #[test]
    fn tab_in_command_mode_completes_single_match() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut s = test_state();
        for (code, mods) in [
            (KeyCode::Char(':'), KeyModifiers::NONE),
            (KeyCode::Char('q'), KeyModifiers::NONE),
            (KeyCode::Char('u'), KeyModifiers::NONE),
            (KeyCode::Char('i'), KeyModifiers::NONE),
            (KeyCode::Tab, KeyModifiers::NONE),
        ] {
            s.handle_key_event(crossterm::event::KeyEvent::new(code, mods))
                .unwrap();
        }
        let v = s.eval_lisp("(command-prefix)").unwrap();
        assert_eq!(v.display(), "quit");
    }

    #[test]
    fn longest_common_prefix_builtin_matches_helper() {
        let mut s = test_state();
        let v = s
            .eval_lisp(r#"(longest-common-prefix ["edit" "editor" "edits"])"#)
            .unwrap();
        assert_eq!(v.display(), "edit");
    }

    #[test]
    fn command_submit_via_minibuffer_does_not_recurse() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut s = test_state();
        for (code, mods) in [
            (KeyCode::Char(':'), KeyModifiers::NONE),
            (KeyCode::Char('q'), KeyModifiers::NONE),
            (KeyCode::Char('u'), KeyModifiers::NONE),
            (KeyCode::Char('i'), KeyModifiers::NONE),
            (KeyCode::Char('t'), KeyModifiers::NONE),
            (KeyCode::Enter, KeyModifiers::NONE),
        ] {
            s.handle_key_event(crossterm::event::KeyEvent::new(code, mods))
                .unwrap();
        }
        assert!(s.quit_requested());
    }

    #[test]
    fn face_define_then_face_of_round_trips() {
        let mut s = test_state();
        s.eval_lisp(r#"(face-define "header" {"fg": 'cyan "bold": 1})"#)
            .unwrap();
        let v = s.eval_lisp(r#"(face-of "header")"#).unwrap();
        assert!(matches!(&*v, Value::Map(m) if !m.is_empty()));
    }

    #[test]
    fn rgb_builtin_round_trips_through_color_from_value() {
        let mut s = test_state();
        let v = s.eval_lisp("(rgb 60 90 130)").unwrap();
        let c = rizz_ui::styling::color_from_value(&v).unwrap();
        assert_eq!(c, Some(rizz_ui::styling::Color::Rgb(60, 90, 130)));
    }

    #[test]
    fn span_builtin_emits_text_and_style_fields() {
        let mut s = test_state();
        let v = s.eval_lisp(r#"(w-span "hi" 'header)"#).unwrap();
        match &*v {
            Value::Map(m) => {
                let text = m
                    .get(&Rc::new(Value::Str("text".into())))
                    .expect("text field");
                assert_eq!(text.as_str().as_deref(), Some("hi"));
            }
            other => panic!("expected map, got {other:?}"),
        }
    }

    #[test]
    fn set_frame_installs_user_layout() {
        let mut s = test_state();
        s.eval_lisp(r#"(fn _star () (w-span "★" ()))"#).unwrap();
        s.eval_lisp(r#"(set-frame _star)"#).unwrap();
        let (_, err) = s.precompute_frame();
        assert!(err.is_none(), "no frame errors: {err:?}");
    }

    #[test]
    fn default_style_lisp_loads_clean() {
        let mut s = test_state();
        let src = include_str!("../../../../init.rz");
        s.eval_lisp_script(src)
            .map_err(|e| format!("init.rz eval failed: {e}"))
            .unwrap();
    }

    #[test]
    fn render_phase_blocks_mutating_builtins() {
        let mut s = test_state();
        s.eval_lisp(r#"(fn _bad () (do (insert-char "x") (text "")))"#)
            .unwrap();
        s.eval_lisp(r#"(set-frame _bad)"#).unwrap();
        let pre = s.focused_buf().text();
        let (_, err) = s.precompute_frame();
        assert!(err.is_some(), "expected a render-phase error");
        let after = s.focused_buf().text();
        assert_eq!(pre, after, "render callback must not mutate the buffer");
    }

    #[test]
    fn default_lisp_binds_normal_mode_keys() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut s = test_state();
        s.eval_lisp("(set-mode 'insert)").unwrap();
        s.eval_lisp("(insert \"ab\")").unwrap();
        s.eval_lisp("(newline)").unwrap();
        s.eval_lisp("(insert \"cd\")").unwrap();
        s.eval_lisp("(set-mode 'normal)").unwrap();
        s.eval_lisp("(move-cursor 'file-start)").unwrap();
        s.handle_key_event(crossterm::event::KeyEvent::new(
            KeyCode::Char('j'),
            KeyModifiers::NONE,
        ))
        .unwrap();
        let b = s.focused_buf();
        assert_eq!(b.cursor_pos().row as i64 + b.file_pos().row as i64, 1);
    }

    /// Regression for the NativeFn::Impure semantics: `evaluate` runs the
    /// focused buffer's text and the bindings it introduces must persist into
    /// subsequent evals.
    #[test]
    fn evaluate_persists_top_level_bindings() {
        let mut s = test_state();
        s.eval_lisp("(set-mode 'insert)").unwrap();
        s.eval_lisp(r#"(insert "(let regress-val 42)")"#).unwrap();
        s.eval_lisp("(evaluate)").unwrap();
        let v = s.eval_lisp("regress-val").unwrap();
        assert_eq!(*v, Value::Int(42));
    }
}
