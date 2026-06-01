//! Embedded lisp runtime (risp) bridged to the editor.
//!
//! The runtime owns a persistent [`Env`] of bindings — including every editor
//! primitive exposed as a native function — and is threaded through one entry
//! point on `State` (`State::eval_lisp` / `eval_lisp_value`). Editor primitives
//! access mutable `State` via an RAII-guarded thread-local pointer (see
//! [`with_editor_mut`]): the runtime is moved out of `State` for the duration
//! of an eval, so there is never simultaneous aliasing of `&mut State`.

use std::cell::Cell;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::rc::Rc;
use std::str::FromStr;

use risp::RispError;
use risp::runtime::{self, Env, NativeFn, RuntimeError, Value};

use crate::action::Action;
use crate::buffer::MoveKind;
use crate::keymap::KeyEvent;
use crate::mode::EditingMode;
use crate::position::Position;
use crate::state::State;
use crate::window::{FocusDir, SplitDir};

// ---------------------------------------------------------------------------
// Editor bridge: thread-local pointer to the live `State`.
// ---------------------------------------------------------------------------

thread_local! {
    static EDITOR: Cell<Option<NonNull<State>>> = const { Cell::new(None) };
}

/// RAII guard: stashes `state` in the thread-local for the lifetime of the
/// guard, restoring the previous value on drop. Only the public `eval_*`
/// entry points on `State` construct one of these, and they hold `&mut self`
/// for the duration, so no other Rust code can observe the aliased pointer.
pub(crate) struct EditorGuard {
    prev: Option<NonNull<State>>,
}

impl EditorGuard {
    pub(crate) fn new(state: &mut State) -> Self {
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
/// the dynamic scope of an `EditorGuard` — that would only happen if a native
/// function is invoked from outside a `State::eval_lisp*` call.
fn with_editor_mut<R>(f: impl FnOnce(&mut State) -> R) -> R {
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

pub struct LispRuntime {
    env: Env,
}

impl LispRuntime {
    pub fn new() -> Self {
        let env = risp::prelude::install(builtins());
        Self { env }
    }

    /// Parse `src` as one top-level form, evaluate it, and update `self.env`
    /// with any new bindings the form introduced.
    pub fn eval_str(&mut self, src: &str) -> Result<Rc<Value>, RispError> {
        let (v, env) = risp::parse_and_run_with_env(src.as_bytes(), &self.env)?;
        self.env = env;
        Ok(v)
    }

    /// Evaluate an already-parsed form. Used by `Action::EvalLisp` so that
    /// keybindings don't re-parse on every keystroke.
    pub fn eval_value(&mut self, form: Rc<Value>) -> Result<Rc<Value>, RispError> {
        let (v, env) = runtime::eval(form, &self.env)?;
        self.env = env;
        Ok(v)
    }

    /// Evaluate a multi-form script: `;`-introduced line comments are stripped
    /// and each top-level form is parsed and evaluated in sequence. Stops on
    /// the first error and returns it.
    pub fn eval_script(&mut self, src: &str) -> Result<(), RispError> {
        self.eval_str(src)?;
        Ok(())
    }
}

impl Default for LispRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolve `~/.config/editor/init.lisp` (or `$XDG_CONFIG_HOME/editor/init.lisp`).
/// Returns `None` if no home directory is known.
pub fn init_script_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(dir.join("editor").join("init.lisp"))
}

// ---------------------------------------------------------------------------
// Minibuffer input → lisp source
// ---------------------------------------------------------------------------

/// Translate ergonomic minibuffer input into a lisp form.
///
/// * `(...)` passes through as-is.
/// * A bare integer becomes `(line N)` — preserves the legacy `:42` jump.
/// * Anything else is wrapped in parens.
///   `head arg1 arg2 ...` and becomes `(head arg1 arg2 ...)`.
/// * Empty input becomes `()` (a no-op).
pub fn wrap_shell_style(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return "()".into();
    }
    if trimmed.starts_with('(') {
        return trimmed.into();
    }
    if let Ok(n) = trimmed.parse::<i64>() {
        return format!("(line {n})");
    }
    let mut parts = trimmed.split_whitespace();
    let head = parts.next().unwrap();
    let mut out = String::with_capacity(trimmed.len() + 4);
    out.push('(');
    out.push_str(head);
    for arg in parts {
        out.push(' ');
        out.push_str(arg);
    }
    out.push(')');
    out
}

// ---------------------------------------------------------------------------
// Builtin registration
// ---------------------------------------------------------------------------

fn builtins() -> Env {
    let mut entries: Vec<(&str, NativeFn)> = Vec::new();
    macro_rules! b {
        ($name:expr, $nargs:expr, $f:expr) => {
            entries.push(($name, NativeFn::impure($name.into(), $nargs, $f)));
        };
    }

    // mode + lifecycle
    b!("q", 0, |_, env| {
        apply(Action::Quit);
        ok_unit(env)
    });

    b!("quit", 0, |_, env| {
        apply(Action::Quit);
        ok_unit(env)
    });

    b!("set-mode", 1, |args, env| {
        let mode = parse_mode_ident(&args[0])?;
        apply(Action::SetMode(mode));
        ok_unit(env)
    });

    // text editing
    b!("insert-char", 1, |args, env| {
        let s = as_str(&args[0], "insert-char")?;
        let c = s
            .chars()
            .next()
            .ok_or_else(|| str_mismatch("insert-char", "non-empty str"))?;
        apply(Action::InsertChar(c));
        ok_unit(env)
    });
    b!("insert", 1, |args, env| {
        let s = as_str(&args[0], "insert")?;
        with_editor_mut(|st| {
            for c in s.chars() {
                let _ = st.apply(&[Rc::new(Action::InsertChar(c))]);
            }
        });
        ok_unit(env)
    });
    b!("delete-char", 0, |_, env| {
        apply(Action::DeleteChar);
        ok_unit(env)
    });
    b!("newline", 0, |_, env| {
        apply(Action::InsertNewline);
        ok_unit(env)
    });

    // cursor movement
    b!("move-cursor", 1, |args, env| {
        let sym = as_ident(&args[0], "move-cursor")?;
        let mk = move_kind(&sym)?;
        apply(Action::MoveCursor(mk));
        ok_unit(env)
    });
    b!("move-cursor-rel", 2, |args, env| {
        let dx = as_int(&args[0], "move-cursor-rel")?;
        let dy = as_int(&args[1], "move-cursor-rel")?;
        let mk = MoveKind::Relative(Position::new(dx as i16, dy as i16));
        apply(Action::MoveCursor(mk));
        ok_unit(env)
    });
    b!("line", 1, |args, env| {
        let n = as_int(&args[0], "line")?;
        let mk = MoveKind::LineNum(n.max(0) as usize);
        apply(Action::MoveCursor(mk));
        ok_unit(env)
    });

    // buffer management
    b!("buf-create", 0, |_, env| {
        apply(Action::BufCreate {
            set_active: true,
            path: None,
        });
        ok_unit(env)
    });
    b!("buf-delete", 0, |_, env| {
        apply(Action::BufDelete);
        ok_unit(env)
    });
    b!("buf-next", 0, |_, env| {
        apply(Action::BufNext);
        ok_unit(env)
    });
    b!("buf-prev", 0, |_, env| {
        apply(Action::BufPrev);
        ok_unit(env)
    });
    b!("edit", 1, |args, env| {
        let p = as_str(&args[0], "edit")?;
        let path = std::path::PathBuf::from_str(&p).unwrap();
        apply(Action::BufEdit(path.into()));
        ok_unit(env)
    });
    b!("write", 0, |_, env| {
        apply(Action::BufWrite(None));
        ok_unit(env)
    });
    b!("write-as", 1, |args, env| {
        let p = as_str(&args[0], "write-as")?;
        let path = std::path::PathBuf::from_str(&p).unwrap();
        apply(Action::BufWrite(Some(path.into())));
        ok_unit(env)
    });

    // windows
    b!("window-split", 1, |args, env| {
        let dir = match as_ident(&args[0], "window-split")?.as_ref() {
            "vertical" => SplitDir::Vertical,
            "horizontal" => SplitDir::Horizontal,
            other => return Err(unknown_variant("window-split", other)),
        };
        apply(Action::WindowSplit(dir));
        ok_unit(env)
    });
    b!("window-close", 0, |_, env| {
        apply(Action::WindowClose);
        ok_unit(env)
    });
    b!("window-focus", 1, |args, env| {
        let dir = match as_ident(&args[0], "window-focus")?.as_ref() {
            "left" => FocusDir::Left,
            "right" => FocusDir::Right,
            "up" => FocusDir::Up,
            "down" => FocusDir::Down,
            other => return Err(unknown_variant("window-focus", other)),
        };
        apply(Action::WindowFocus(dir));
        ok_unit(env)
    });
    b!("window-focus-next", 0, |_, env| {
        apply(Action::WindowFocusNext);
        ok_unit(env)
    });

    // keymap (lisp owns the keymap; lhs is parsed via `KeyEvent::parse_sequence`)
    b!("keymap-set", 3, |args, env| {
        let mode = parse_mode_ident(&args[0])?;
        let lhs_str = as_str(&args[1], "keymap-set")?;
        let lhs =
            KeyEvent::parse_sequence(&lhs_str).map_err(|e| str_mismatch_msg("keymap-set", &e))?;
        let form = args[2].clone();
        apply(Action::KeymapSet {
            mode,
            lhs,
            rhs: Rc::new(Action::EvalLisp(form)),
        });
        ok_unit(env)
    });
    b!("keymap-remove", 2, |args, env| {
        let mode = parse_mode_ident(&args[0])?;
        let lhs_str = as_str(&args[1], "keymap-remove")?;
        let lhs = KeyEvent::parse_sequence(&lhs_str)
            .map_err(|e| str_mismatch_msg("keymap-remove", &e))?;
        apply(Action::KeymapRemove { mode, lhs });
        ok_unit(env)
    });

    // minibuffer flow
    //
    // `command-submit` runs inside an `eval_lisp_value` frame (it's fired from
    // the `<enter>` keybinding in command mode), so it cannot call back into
    // `State::eval_lisp` — that would re-take `self.lisp` and panic. Parse and
    // evaluate the user's input directly against `env` instead.
    b!("command-submit", 0, |_, env| {
        let cmd = with_editor_mut(|st| st.take_minibuffer_command());
        let src = wrap_shell_style(&cmd);
        match risp::parse_and_run_with_env(src.as_bytes(), env) {
            Ok((v, new_env)) => {
                if !v.is_unit() {
                    with_editor_mut(|st| st.set_minibuffer_message(&v.display()));
                }
                Ok((unit(), new_env))
            }
            Err(e) => {
                let msg = e.to_string();
                with_editor_mut(|st| st.set_minibuffer_message(&msg));
                ok_unit(env)
            }
        }
    });

    b!("command-cancel", 0, |_, env| {
        apply(Action::CommandCancel);
        ok_unit(env)
    });

    // eval
    b!("eval-buffer", 0, |_, env| {
        let src = with_editor_mut(|st| {
            st.focused_buf()
                .selected_text()
                .unwrap_or_else(|| st.focused_buf().text())
        });
        match risp::parse_and_run_with_env(src.as_bytes(), env) {
            Ok((v, new_env)) => {
                if !v.is_unit() {
                    with_editor_mut(|st| st.set_minibuffer_message(&v.display()));
                }
                Ok((unit(), new_env))
            }
            Err(e) => {
                let msg = e.to_string();
                with_editor_mut(|st| st.set_minibuffer_message(&msg));
                ok_unit(env)
            }
        }
    });

    // user-facing messaging
    b!("message", 1, |args, env| {
        let s = as_str(&args[0], "message")?;
        with_editor_mut(|st| st.set_minibuffer_message(&s));
        ok_unit(env)
    });

    // queries
    b!("buffer-text", 0, |_, env| {
        let s = with_editor_mut(|st| st.focused_buf().text());
        Ok((Rc::new(s.into()), env.clone()))
    });

    b!("selected-text", 0, |_, env| {
        let s = with_editor_mut(|st| st.focused_buf().selected_text());
        Ok((Rc::new(s.into()), env.clone()))
    });

    b!("cursor-line", 0, |_, env| {
        let n = with_editor_mut(|st| {
            let b = st.focused_buf();
            b.cursor_pos().row as i64 + b.file_pos().row as i64
        });
        Ok((Rc::new(n.into()), env.clone()))
    });

    b!("cursor-col", 0, |_, env| {
        let n = with_editor_mut(|st| {
            let b = st.focused_buf();
            b.cursor_pos().col as i64 + b.file_pos().col as i64
        });
        Ok((Rc::new(n.into()), env.clone()))
    });

    b!("focused-mode", 0, |_, env| {
        let m = with_editor_mut(|st| st.focused_buf().mode());
        let s: &str = match m {
            EditingMode::Normal => "normal",
            EditingMode::Insert => "insert",
            EditingMode::Visual => "visual",
            EditingMode::VisualLine => "visual-line",
            EditingMode::VisualBlock => "visual-block",
            EditingMode::Command => "command",
        };
        Ok((Rc::new(Value::Ident(s.into())), env.clone()))
    });

    Env::of_builtins(entries)
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn unit() -> Rc<Value> {
    Rc::new(Value::Unit)
}

fn ok_unit(env: &Env) -> Result<(Rc<Value>, Env), RuntimeError> {
    Ok((unit(), env.clone()))
}

fn apply(action: Action) {
    with_editor_mut(|st| {
        let _ = st.apply(&[Rc::new(action)]);
    });
}

fn as_str(v: &Rc<Value>, name: &str) -> Result<Rc<str>, RuntimeError> {
    v.as_str()
        .ok_or_else(|| RuntimeError::type_mismatch(name, "str", v))
}

fn as_int(v: &Rc<Value>, name: &str) -> Result<i64, RuntimeError> {
    v.as_int()
        .ok_or_else(|| RuntimeError::type_mismatch(name, "int", v))
}

fn as_ident(v: &Rc<Value>, name: &str) -> Result<Rc<str>, RuntimeError> {
    match &**v {
        Value::Ident(s) => Ok(s.clone()),
        _ => Err(RuntimeError::type_mismatch(name, "ident", v)),
    }
}

fn parse_mode_ident(v: &Rc<Value>) -> Result<EditingMode, RuntimeError> {
    let s = as_ident(v, "mode")?;
    Ok(match s.as_ref() {
        "normal" => EditingMode::Normal,
        "insert" => EditingMode::Insert,
        "visual" => EditingMode::Visual,
        "visual-line" => EditingMode::VisualLine,
        "visual-block" => EditingMode::VisualBlock,
        "command" => EditingMode::Command,
        other => return Err(unknown_variant("mode", other)),
    })
}

fn move_kind(sym: &str) -> Result<MoveKind, RuntimeError> {
    use MoveKind as M;
    Ok(match sym {
        "down" => M::Relative(Position::new(0, 1)),
        "up" => M::Relative(Position::new(0, -1)),
        "left" => M::Relative(Position::new(-1, 0)),
        "right" => M::Relative(Position::new(1, 0)),
        "line-start" => M::LineStart,
        "line-end" => M::LineEnd,
        "file-start" => M::FileStart,
        "file-end" => M::FileEnd,
        "word-start" => M::WordStart,
        "word-end" => M::WordEnd,
        "half-page-down" => M::HalfPageDown,
        "half-page-up" => M::HalfPageUp,
        "center" => M::Center,
        other => return Err(unknown_variant("move-cursor", other)),
    })
}

fn unknown_variant(name: &str, got: &str) -> RuntimeError {
    RuntimeError::TypeMismatch {
        name: name.into(),
        expected: "known symbol".into(),
        got: got.into(),
    }
}

fn str_mismatch(name: &str, expected: &str) -> RuntimeError {
    RuntimeError::TypeMismatch {
        name: name.into(),
        expected: expected.into(),
        got: "?".into(),
    }
}

fn str_mismatch_msg(name: &str, msg: &str) -> RuntimeError {
    RuntimeError::TypeMismatch {
        name: name.into(),
        expected: "valid key sequence".into(),
        got: msg.into(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
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
    fn wrap_shell_style_translates_input() {
        assert_eq!(wrap_shell_style("quit"), "(quit)");
        assert_eq!(wrap_shell_style("edit foo.txt"), "(edit foo.txt)");
        assert_eq!(wrap_shell_style("(+ 1 2)"), "(+ 1 2)");
        assert_eq!(wrap_shell_style("+ 1 2"), "(+ 1 2)");
        assert_eq!(wrap_shell_style("42"), "(line 42)");
        assert_eq!(wrap_shell_style("   "), "()");
    }

    #[test]
    fn default_lisp_binds_normal_mode_keys() {
        // `j` is bound to (move-cursor 'down) in default.lisp. If the bundled
        // script failed to load, `j` would not bind and this would no-op.
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut s = test_state();
        // Seed two lines so we can observe a downward move.
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
}
