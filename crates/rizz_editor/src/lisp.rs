//! Embedded lisp runtime (rizz) bridged to the editor.
//!
//! The runtime owns a persistent [`Env`] of bindings — including every editor
//! primitive exposed as a native function — and is threaded through one entry
//! point on `State` (`State::eval_lisp` / `eval_lisp_value`). Editor primitives
//! access mutable `State` via an RAII-guarded thread-local pointer (see
//! `with_editor_mut`): the runtime is moved out of `State` for the duration
//! of an eval, so there is never simultaneous aliasing of `&mut State`.

use std::cell::Cell;
use std::path::PathBuf;
use std::process;
use std::ptr::NonNull;
use std::rc::Rc;
use std::str::FromStr;

use anyhow::anyhow;
use im::{HashMap as ImHashMap, Vector};
use rizz::runtime::{Env, NativeFn, RuntimeError, Value};
use rizz::{RizzError, Runtime};

use rizz_actions::Action;
use rizz_core::{EditingMode, FocusDir, Position, SplitDir};
use rizz_input::KeyEvent;
use rizz_text::{MoveKind, props::PropEntry, wrap::WrapMode};
use rizz_ui::{
    popup::{Dim, Placement, Side},
    styling::{normalize_style_value, rgb_value, style_from_value, style_to_value},
    widget::parse_widget,
};

use crate::state::{PopupSpec, State};

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

fn in_render_phase() -> bool {
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

pub struct LispRuntime(Runtime);

impl LispRuntime {
    pub fn new() -> Self {
        let env = builtins().union(rizz::prelude::env());
        Self(Runtime::with_env(env))
    }

    pub fn eval_str(&mut self, src: &str) -> Result<Rc<Value>, RizzError> {
        self.0.eval(src.as_bytes())
    }

    pub fn eval_value(&mut self, form: Rc<Value>) -> Result<Rc<Value>, RizzError> {
        Ok(self.0.eval_form(form)?)
    }

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

/// Resolve `~/.config/editor/init.lisp` (or `$XDG_CONFIG_HOME/editor/init.lisp`).
pub fn init_script_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(dir.join("editor").join("init.lisp"))
}

// ---------------------------------------------------------------------------
// Builtin registration
// ---------------------------------------------------------------------------

fn builtins() -> Env {
    let mut entries: Vec<(&str, NativeFn)> = Vec::new();
    let mut aliases: Vec<(&str, &str)> = Vec::new();
    macro_rules! b {
        ($name:expr, $nargs:expr, $f:expr) => {
            entries.push(($name, NativeFn::impure($name.into(), $nargs, $f)));
        };
        ($name:expr, $nargs:expr, $f:expr, $doc:expr) => {
            entries.push((
                $name,
                NativeFn::impure($name.into(), $nargs, $f).with_doc(Rc::from($doc)),
            ));
        };
    }
    macro_rules! alias {
        ($a:expr => $t:expr) => {
            aliases.push(($a, $t));
        };
    }

    // mode + lifecycle
    b!(
        "quit",
        0,
        |_, env| {
            apply(Action::Quit)?;
            ok_unit(env)
        },
        "(quit/0)\nexit the application"
    );
    alias!("q" => "quit");

    b!(
        "set-mode",
        1,
        |args, env| {
            let mode = parse_mode_ident(&args[0])?;
            apply(Action::SetMode(mode))?;
            ok_unit(env)
        },
        "(set-mode/1)\nchange the editing mode.\naccepts one of: 'normal | 'insert | 'visual | 'visual-line | 'visual-block | 'command"
    );

    // text editing
    b!("insert-char", 1, |args, env| {
        let s = as_str(&args[0], "insert-char")?;
        let c = s
            .chars()
            .next()
            .ok_or_else(|| str_mismatch("insert-char", "non-empty str"))?;
        apply(Action::InsertChar(c))?;
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
        apply(Action::DeleteChar)?;
        ok_unit(env)
    });

    b!("delete-char-at", 2, |args, env| {
        let col = as_usize(&args[0], "delete-char-at")?;
        let row = as_usize(&args[1], "delete-char-at")?;
        apply(Action::DeleteCharAt(Position::new(col, row)))?;
        ok_unit(env)
    });
    b!("delete-selection", 0, |_, env| {
        apply(Action::DeleteSelection)?;
        ok_unit(env)
    });
    b!("delete-line", 0, |_, env| {
        let count = with_editor_mut(|st| st.pending_count_or_one());
        apply(Action::DeleteLine { count })?;
        ok_unit(env)
    });
    b!("delete-motion", 1, |args, env| {
        let sym = as_ident(&args[0], "delete-motion")?;
        let kind = MoveKind::from_str(&sym).map_err(|_| unknown_variant("delete-motion", &sym))?;
        let count = with_editor_mut(|st| st.pending_count_or_one());
        apply(Action::DeleteMotion { kind, count })?;
        ok_unit(env)
    });
    b!("newline", 0, |_, env| {
        apply(Action::InsertNewline)?;
        ok_unit(env)
    });
    b!("undo", 0, |_, env| {
        apply(Action::Undo)?;
        ok_unit(env)
    });
    b!("redo", 0, |_, env| {
        apply(Action::Redo)?;
        ok_unit(env)
    });

    // cursor movement
    b!("move-cursor", 1, |args, env| {
        let sym = as_ident(&args[0], "move-cursor")?;
        let mk = MoveKind::from_str(&sym).map_err(|_| unknown_variant("move-cursor", &sym))?;
        let count = with_editor_mut(|st| st.pending_count_or_one());
        apply(Action::MoveCursor { kind: mk, count })?;
        ok_unit(env)
    });
    b!("move-cursor-rel", 2, |args, env| {
        let dx = as_int(&args[0], "move-cursor-rel")?;
        let dy = as_int(&args[1], "move-cursor-rel")?;
        let mk = MoveKind::Relative(Position::new(dx as i16, dy as i16));
        let count = with_editor_mut(|st| st.pending_count_or_one());
        apply(Action::MoveCursor { kind: mk, count })?;
        ok_unit(env)
    });
    b!("line", 1, |args, env| {
        let n = as_int(&args[0], "line")?;
        let mk = MoveKind::LineNum(n.max(0) as usize);
        apply(Action::MoveCursor { kind: mk, count: 1 })?;
        ok_unit(env)
    });

    // buffer management
    b!("buf-create", 0, |_, env| {
        apply(Action::BufCreate {
            set_active: true,
            path: None,
        })?;
        ok_unit(env)
    });
    alias!("bc" => "buf-create");
    b!("buf-delete", 0, |_, env| {
        apply(Action::BufDelete)?;
        ok_unit(env)
    });
    alias!("bd" => "buf-delete");
    b!("buf-next", 0, |_, env| {
        apply(Action::BufNext)?;
        ok_unit(env)
    });
    alias!("bn" => "buf-next");
    b!("buf-prev", 0, |_, env| {
        apply(Action::BufPrev)?;
        ok_unit(env)
    });
    alias!("bp" => "buf-prev");
    b!("edit", 1, |args, env| {
        let p = as_str(&args[0], "edit")?;
        let path = std::path::PathBuf::from_str(&p).unwrap();
        apply(Action::BufEdit(path.into()))?;
        ok_unit(env)
    });
    alias!("e" => "edit");
    b!("write", 0, |_, env| {
        apply(Action::BufWrite(None))?;
        ok_unit(env)
    });
    alias!("w" => "write");
    b!("write-as", 1, |args, env| {
        let p = as_str(&args[0], "write-as")?;
        let path = std::path::PathBuf::from_str(&p).unwrap();
        apply(Action::BufWrite(Some(path.into())))?;
        ok_unit(env)
    });

    // windows
    b!("window-split", 1, |args, env| {
        let dir = match as_ident(&args[0], "window-split")?.as_ref() {
            "vertical" | "v" => SplitDir::Vertical,
            "horizontal" | "h" => SplitDir::Horizontal,
            other => return Err(unknown_variant("window-split", other)),
        };
        apply(Action::WindowSplit(dir))?;
        ok_unit(env)
    });
    b!("window-close", 0, |_, env| {
        apply(Action::WindowClose)?;
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
        apply(Action::WindowFocus(dir))?;
        ok_unit(env)
    });
    b!("window-focus-next", 0, |_, env| {
        apply(Action::WindowFocusNext)?;
        ok_unit(env)
    });

    // keymap
    b!("keymap-set", 3, |args, env| {
        let mode = parse_mode_name(&args[0])?;
        let lhs_str = as_str(&args[1], "keymap-set")?;
        let lhs =
            KeyEvent::parse_sequence(&lhs_str).map_err(|e| str_mismatch_msg("keymap-set", &e))?;
        let form = args[2].clone();
        apply(Action::KeymapSet {
            mode,
            lhs,
            rhs: Rc::new(Action::EvalLisp(form)),
        })?;
        ok_unit(env)
    });

    b!("keymap-remove", 2, |args, env| {
        let mode = parse_mode_name(&args[0])?;
        let lhs_str = as_str(&args[1], "keymap-remove")?;
        let lhs = KeyEvent::parse_sequence(&lhs_str)
            .map_err(|e| str_mismatch_msg("keymap-remove", &e))?;
        apply(Action::KeymapRemove { mode, lhs })?;
        ok_unit(env)
    });

    b!("keymap-get", 1, |args, env| {
        let mode = parse_mode_name(&args[0])?;
        let mappings = with_editor_mut(|st| {
            st.keymap_registry()
                .iter()
                .filter(|(m, _, _)| m == &mode)
                .map(|(m, p, a)| {
                    let lhs: String = p.iter().map(|e| e.to_string()).collect::<Vec<_>>().concat();
                    let rhs: Value = match a {
                        Action::EvalLisp(form) => (**form).clone(),
                        other => format!("{:?}", other).into(),
                    };
                    let entry: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::from_iter([
                        (Rc::new(Value::Str("mode".into())), Rc::new(Value::Str(m))),
                        (
                            Rc::new(Value::Str("lhs".into())),
                            Rc::new(Value::Str(lhs.into())),
                        ),
                        (Rc::new(Value::Str("rhs".into())), Rc::new(rhs)),
                    ]);
                    Value::Map(entry)
                })
                .collect::<Vec<Value>>()
        });
        Ok((Rc::new(mappings.into()), env.clone()))
    });

    // minibuffer flow
    b!("command-submit", 0, |_, env| {
        let cmd = with_editor_mut(|st| st.take_minibuffer_command());
        with_editor_mut(|st| st.record_cmd(&cmd));
        let src = wrap_shell_style(&cmd);
        match rizz::parse_and_run_with_env(src.as_bytes(), env) {
            Ok((v, new_env)) => {
                if !v.is_unit() {
                    notify_via_env(&v.display(), &new_env);
                }
                Ok((unit(), new_env))
            }
            Err(e) => {
                notify_via_env(&e.to_string(), env);
                ok_unit(env)
            }
        }
    });

    b!("command-cancel", 0, |_, env| {
        apply(Action::CommandCancel)?;
        ok_unit(env)
    });

    b!("evaluate", 0, |_, env| {
        let src = {
            with_editor_mut(|st| {
                st.focused_buf()
                    .selected_text()
                    .unwrap_or_else(|| st.focused_buf().text())
            })
        };
        match rizz::parse_and_run_with_env(src.as_bytes(), env) {
            Ok((v, new_env)) => {
                if !v.is_unit() {
                    notify_via_env(&v.display(), &new_env);
                }
                Ok((unit(), new_env))
            }
            Err(e) => {
                notify_via_env(&e.to_string(), env);
                ok_unit(env)
            }
        }
    });

    b!("notify-record", 1, |args, env| {
        let s = as_str(&args[0], "notify-record")?;
        with_editor_mut(|st| st.record_message(&s));
        ok_unit(env)
    });
    b!("message-history", 0, |_, env| {
        let msgs: Vector<Rc<Value>> = with_editor_mut(|st| {
            st.message_history()
                .map(|s| Rc::new(Value::Str(s.clone())))
                .collect()
        });
        Ok((Rc::new(Value::Array(msgs)), env.clone()))
    });
    b!("command-history", 0, |_, env| {
        let cmds: Vector<Rc<Value>> = with_editor_mut(|st| {
            st.cmd_history()
                .map(|s| Rc::new(Value::Str(s.clone())))
                .collect()
        });
        Ok((Rc::new(Value::Array(cmds)), env.clone()))
    });

    // popups
    b!("popup-open", 1, |args, env| {
        let widget = with_editor_mut(|st| {
            let theme = st.theme().borrow();
            parse_widget(&args[0], &theme)
        })?;
        let mut spec = PopupSpec::new(widget);
        if let Some(opts) = args.get(1) {
            parse_popup_options(opts, &mut spec)?;
        }
        let bufno = with_editor_mut(|st| st.open_popup(spec));
        Ok((Rc::new(Value::Int(bufno as i64)), env.clone()))
    });
    b!("popup-close", 0, |_, env| {
        let closed = with_editor_mut(|st| st.close_popup());
        Ok((Rc::new(Value::Int(closed as i64)), env.clone()))
    });
    b!("popup-bufno", 0, |_, env| {
        let v = with_editor_mut(|st| {
            st.top_popup_bufno()
                .map(|n| Value::Int(n as i64))
                .unwrap_or(Value::Unit)
        });
        Ok((Rc::new(v), env.clone()))
    });
    b!("minibuffer-bufno", 0, |_, env| {
        let n = with_editor_mut(|st| st.minibuffer_bufno());
        Ok((Rc::new(Value::Int(n as i64)), env.clone()))
    });
    b!("popup-mode", 0, |_, env| {
        let v = with_editor_mut(|st| st.top_popup_mode().map(Value::Str).unwrap_or(Value::Unit));
        Ok((Rc::new(v), env.clone()))
    });
    b!("popup?", 0, |_, env| {
        let v = with_editor_mut(|st| st.has_popup());
        Ok((Rc::new(Value::Int(v as i64)), env.clone()))
    });

    // queries
    b!("buf-text-set", 2, |args, env| {
        let bufno = as_int(&args[0], "buf-text-set")?;
        if bufno < 0 {
            return Err(RuntimeError::type_mismatch(
                "buf-text-set",
                "integer >= 0",
                &args[0],
            ));
        }
        let text = args[1].display();
        let nbufs = with_editor_mut(|st| st.nbufs());
        if bufno as usize >= nbufs {
            return Err(RuntimeError::Other(anyhow!(
                "bad input. editor has {nbufs} 0-indexed buffers but you requested buffer {bufno}"
            )));
        }

        with_editor_mut(|st| st.set_buffer_contents(bufno as usize, &text));
        Ok((unit(), env.clone()))
    });

    b!("buf-text", 0, |_, env| {
        let s = with_editor_mut(|st| st.focused_buf().text());
        Ok((Rc::new(s.into()), env.clone()))
    });

    b!("buf-no", 0, |_, env| {
        let s = with_editor_mut(|st| st.focused_bufno());
        Ok((Rc::new(Value::Int(s as i64)), env.clone()))
    });

    b!("buf-path", 0, |_, env| {
        let v: Value = with_editor_mut(|st| st.focused_buf().fs_path())
            .map(|p| p.to_string_lossy().as_ref().into())
            .map(|s: Rc<str>| Value::Str(s))
            .unwrap_or(Value::Unit);
        Ok((Rc::new(v), env.clone()))
    });
    alias!("%"=>"buf-path");

    b!("selected-text", 0, |_, env| {
        let s = with_editor_mut(|st| st.focused_buf().selected_text());
        Ok((Rc::new(s.into()), env.clone()))
    });

    b!("cursor-line", 0, |_, env| {
        let n = with_editor_mut(|st| st.focused_buf().abs_row() as i64);
        Ok((Rc::new(n.into()), env.clone()))
    });

    b!("line-at", 1, |args, env| {
        let idx = as_usize(&args[0], "line-at")?;
        let s = with_editor_mut(|st| st.focused_buf().lines_at(idx).next().map(|s| s.to_string()));
        Ok((Rc::new(s.into()), env.clone()))
    });

    b!("cursor-col", 0, |_, env| {
        let n = with_editor_mut(|st| st.focused_buf().abs_col() as i64);
        Ok((Rc::new(n.into()), env.clone()))
    });

    // wrap settings
    b!("buffer-wrap", 0, |args, env| {
        if let Some(arg) = args.first() {
            let sym = as_ident_or_str(arg, "buffer-wrap")?;
            let m = WrapMode::from_str(&sym).ok_or_else(|| unknown_variant("buffer-wrap", &sym))?;
            with_editor_mut(|st| st.focused_buf_mut().set_wrap_mode(m));
            ok_unit(env)
        } else {
            let s: Rc<str> = with_editor_mut(|st| st.focused_buf().wrap_mode().as_str().into());
            Ok((Rc::new(Value::Str(s)), env.clone()))
        }
    });
    b!("buffer-wrap?", 0, |_, env| {
        let s: Rc<str> = with_editor_mut(|st| st.focused_buf().wrap_mode().as_str().into());
        Ok((Rc::new(Value::Str(s)), env.clone()))
    });

    b!("buffer-wrap-column", 1, |args, env| {
        let n = as_int(&args[0], "buffer-wrap-column")?;
        let col = if n <= 0 {
            None
        } else {
            Some(n.min(u16::MAX as i64) as u16)
        };
        with_editor_mut(|st| st.focused_buf_mut().set_wrap_column(col));
        ok_unit(env)
    });

    b!("buffer-breakindent", 1, |args, env| {
        let n = as_int(&args[0], "buffer-breakindent")?;
        with_editor_mut(|st| st.focused_buf_mut().set_breakindent(n != 0));
        ok_unit(env)
    });

    // styling: faces + colors
    b!("face-define", 2, |args, env| {
        let name = as_ident_or_str(&args[0], "face-define")?;
        let style = with_editor_mut(|st| {
            let theme = st.theme().borrow();
            style_from_value(&args[1], &theme)
        })?;
        with_editor_mut(|st| {
            st.theme().borrow_mut().insert(name, style);
        });
        ok_unit(env)
    });
    b!("face-of", 1, |args, env| {
        let name = as_ident_or_str(&args[0], "face-of")?;
        let v = with_editor_mut(|st| {
            st.theme()
                .borrow()
                .lookup(&name)
                .map(style_to_value)
                .unwrap_or_else(|| Rc::new(Value::Unit))
        });
        Ok((v, env.clone()))
    });
    b!("rgb", 3, |args, env| {
        let r = as_u8(&args[0], "rgb")?;
        let g = as_u8(&args[1], "rgb")?;
        let b = as_u8(&args[2], "rgb")?;
        Ok((rgb_value(r, g, b), env.clone()))
    });
    b!("span", 2, |args, env| {
        let text = as_str(&args[0], "span")?;
        let style_val = with_editor_mut(|st| {
            let theme = st.theme().borrow();
            normalize_style_value(&args[1], &theme)
        })?;
        let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
        m.insert(
            Rc::new(Value::Str("text".into())),
            Rc::new(Value::Str(text)),
        );
        if !style_val.is_unit() {
            m.insert(Rc::new(Value::Str("style".into())), style_val);
        }
        Ok((Rc::new(Value::Map(m)), env.clone()))
    });

    // ---- widget tree builtins ---------------------------------------------

    b!("set-frame", 1, |args, env| {
        let v = args[0].clone();
        let opt = if v.is_unit() { None } else { Some(v) };
        with_editor_mut(|st| st.set_frame_fn(opt));
        ok_unit(env)
    });

    b!("text", 2, |args, env| {
        let text = as_str(&args[0], "text")?;
        let style_val = with_editor_mut(|st| {
            let theme = st.theme().borrow();
            normalize_style_value(&args[1], &theme)
        })?;
        let mut span: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
        span.insert(strkey("text"), Rc::new(Value::Str(text)));
        if !style_val.is_unit() {
            span.insert(strkey("style"), style_val);
        }
        Ok((Rc::new(Value::Map(span)), env.clone()))
    });

    b!("line", 1, |args, env| {
        let spans: Vec<Rc<Value>> = value_iter(&args[0]).collect();
        Ok((widget_line(spans), env.clone()))
    });

    b!("right-align", 1, |args, env| {
        Ok((widget_set_align(args[0].clone(), "right"), env.clone()))
    });
    b!("center-align", 1, |args, env| {
        Ok((widget_set_align(args[0].clone(), "center"), env.clone()))
    });

    b!("vstack", 1, |args, env| {
        Ok((widget_stack("vertical", &args[0]), env.clone()))
    });
    b!("hstack", 1, |args, env| {
        Ok((widget_stack("horizontal", &args[0]), env.clone()))
    });

    b!("cells", 2, |args, env| {
        let n = as_int(&args[0], "cells")?.max(0).min(u16::MAX as i64);
        Ok((
            widget_constrained("cells", n, 1, args[1].clone()),
            env.clone(),
        ))
    });
    b!("min-cells", 2, |args, env| {
        let n = as_int(&args[0], "min-cells")?.max(0).min(u16::MAX as i64);
        Ok((
            widget_constrained("min", n, 1, args[1].clone()),
            env.clone(),
        ))
    });
    b!("fill", 2, |args, env| {
        let n = as_int(&args[0], "fill")?.max(0).min(u16::MAX as i64);
        Ok((
            widget_constrained("fill", n, 1, args[1].clone()),
            env.clone(),
        ))
    });
    b!("frac", 3, |args, env| {
        let n = as_int(&args[0], "frac")?.max(0).min(u16::MAX as i64);
        let m = as_int(&args[1], "frac")?.max(1).min(u16::MAX as i64);
        Ok((
            widget_constrained("frac", n, m, args[2].clone()),
            env.clone(),
        ))
    });

    b!("block", 2, |args, env| {
        let child = args[0].clone();
        let props = match &*args[1] {
            Value::Map(m) => m.clone(),
            Value::Unit => ImHashMap::new(),
            _ => {
                return Err(RuntimeError::type_mismatch(
                    "block.props",
                    "map | ()",
                    &args[1],
                ));
            }
        };
        let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
        m.insert(strkey("type"), Rc::new(Value::Str("block".into())));
        m.insert(strkey("child"), child);
        for k in ["border", "title", "face", "border-face", "title-face"] {
            if let Some(v) = props.get(&strkey(k)) {
                m.insert(strkey(k), v.clone());
            }
        }
        Ok((Rc::new(Value::Map(m)), env.clone()))
    });

    b!("editor-tree", 1, |args, env| {
        let props = match &*args[0] {
            Value::Map(m) => m.clone(),
            Value::Unit => ImHashMap::new(),
            _ => {
                return Err(RuntimeError::type_mismatch(
                    "editor-tree.props",
                    "map | ()",
                    &args[0],
                ));
            }
        };
        let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
        m.insert(strkey("type"), Rc::new(Value::Str("editor-tree".into())));
        if let Some(g) = props.get(&strkey("gutter")) {
            m.insert(strkey("gutter"), g.clone());
        }
        if let Some(w) = props.get(&strkey("gutter-width")) {
            m.insert(strkey("gutter-width"), w.clone());
        }
        Ok((Rc::new(Value::Map(m)), env.clone()))
    });

    b!("minibuffer", 0, |_, env| {
        let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
        m.insert(strkey("type"), Rc::new(Value::Str("minibuffer".into())));
        Ok((Rc::new(Value::Map(m)), env.clone()))
    });

    b!("empty", 0, |_, env| {
        let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
        m.insert(strkey("type"), Rc::new(Value::Str("empty".into())));
        Ok((Rc::new(Value::Map(m)), env.clone()))
    });

    b!("buffer-view", 0, |args, env| {
        let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
        m.insert(strkey("type"), Rc::new(Value::Str("buffer-view".into())));
        if let Some(arg) = args.first() {
            let bufno = as_int(arg, "buffer-view.bufno")?.max(0);
            m.insert(strkey("bufno"), Rc::new(Value::Int(bufno)));
        }
        Ok((Rc::new(Value::Map(m)), env.clone()))
    });

    // text properties + overlays
    b!("put-text-property", 5, |args, env| {
        let start_row = as_usize(&args[0], "put-text-property")?;
        let start_col = as_usize(&args[1], "put-text-property")?;
        let end_row = as_usize(&args[2], "put-text-property")?;
        let end_col = as_usize(&args[3], "put-text-property")?;
        let face = args[4].clone();
        with_editor_mut(|st| {
            st.focused_buf_mut()
                .props_mut()
                .push_text_property(PropEntry {
                    start: Position::new(start_col, start_row),
                    end: Position::new(end_col, end_row),
                    face: Some(face),
                    display: None,
                    priority: 0,
                    pad_to_width: false,
                });
        });
        ok_unit(env)
    });
    b!("clear-text-properties", 0, |_, env| {
        with_editor_mut(|st| {
            st.focused_buf_mut().props_mut().clear_text_properties();
        });
        ok_unit(env)
    });

    b!("overlay-create", 5, |args, env| {
        let start_row = as_usize(&args[0], "overlay-create")?;
        let start_col = as_usize(&args[1], "overlay-create")?;
        let end_row = as_usize(&args[2], "overlay-create")?;
        let end_col = as_usize(&args[3], "overlay-create")?;
        let face = args[4].clone();
        let id = with_editor_mut(|st| {
            st.focused_buf_mut().props_mut().create_overlay(PropEntry {
                start: Position::new(start_col, start_row),
                end: Position::new(end_col, end_row),
                face: Some(face),
                display: None,
                priority: 0,
                pad_to_width: false,
            })
        });
        Ok((Rc::new(Value::Int(id.0 as i64)), env.clone()))
    });
    b!("overlay-put", 3, |args, env| {
        let id = rizz_text::OverlayId(as_int(&args[0], "overlay-put")? as u64);
        let key = as_ident_or_str(&args[1], "overlay-put")?;
        enum Update {
            Face(Rc<Value>),
            Priority(i64),
            PadToWidth(bool),
            Display(Option<rizz_core::Display>),
        }
        let update = match key.as_ref() {
            "face" => Update::Face(args[2].clone()),
            "priority" => Update::Priority(as_int(&args[2], "overlay-put")?),
            "pad-to-width" => Update::PadToWidth(args[2].is_truthy()),
            "display" => Update::Display(display_from_value(&args[2])?),
            other => {
                return Err(RuntimeError::TypeMismatch {
                    name: "overlay-put".into(),
                    expected: "face|priority|pad-to-width|display".into(),
                    got: other.into(),
                });
            }
        };
        with_editor_mut(|st| {
            if let Some(e) = st.focused_buf_mut().props_mut().overlay_mut(id) {
                match update {
                    Update::Face(f) => e.face = Some(f),
                    Update::Priority(p) => e.priority = p,
                    Update::PadToWidth(b) => e.pad_to_width = b,
                    Update::Display(d) => e.display = d,
                }
            }
        });
        ok_unit(env)
    });
    b!("overlay-delete", 1, |args, env| {
        let id = rizz_text::OverlayId(as_int(&args[0], "overlay-delete")? as u64);
        with_editor_mut(|st| {
            st.focused_buf_mut().props_mut().delete_overlay(id);
        });
        ok_unit(env)
    });

    b!("focused-mode", 0, |_, env| {
        let s = with_editor_mut(|st| st.focused_buf().mode().as_str());
        Ok((Rc::new(Value::Str(s.into())), env.clone()))
    });

    b!("last-key", 0, |_, env| {
        let s = with_editor_mut(|st| {
            st.last_key()
                .map(|k| k.code.to_string())
                .unwrap_or_else(|| "None".to_string())
        });
        Ok((Rc::new(Value::Str(s.into())), env.clone()))
    });

    b!("workdir", 0, |_, env| {
        let d: Value = with_editor_mut(|st| st.workdir()).as_ref().into();
        Ok((Rc::new(d), env.clone()))
    });

    b!("fs-canonicalize", 1, |args, env| {
        let s = as_str(&args[0], "fs-canonicalize")?;
        let path = std::fs::canonicalize(s.as_ref())?;
        Ok((Rc::new(path.into()), env.clone()))
    });

    b!("fs-parent", 1, |args, env| {
        let s = as_str(&args[0], "fs-parent")?;
        let path = PathBuf::from_str(&s).unwrap();
        if let Some(parent) = path.parent()
            && parent.exists()
        {
            Ok((Rc::new(parent.into()), env.clone()))
        } else {
            Ok((unit(), env.clone()))
        }
    });

    b!("fs-readdir", 1, |args, env| {
        let path = as_str(&args[0], "fs-readdir")?;
        let dirs = std::fs::read_dir(path.as_ref())?
            .map(|res| res.map(|e| e.path().into()))
            .collect::<Result<Vector<Value>, std::io::Error>>()?;
        Ok((Rc::new(dirs.into()), env.clone()))
    });
    alias!("ls"=>"fs-readdir");
    alias!("readdir"=>"fs-readdir");

    b!("fs-isdir", 1, |args, env| {
        let path = as_str(&args[0], "fs-isdir")?;
        let meta = std::fs::metadata(path.as_ref())?;
        Ok((Rc::new(meta.is_dir().into()), env.clone()))
    });

    b!("exec", 1, |args, env| {
        let cmd_args = as_str(&args[0], "exec")?;
        let mut prog = cmd_args.split_ascii_whitespace();

        let cmd = prog.next().unwrap_or("");
        if cmd.is_empty() {
            return Err(RuntimeError::type_mismatch(
                "exec",
                "non-empty string",
                &args[0],
            ));
        }
        let output = process::Command::new(cmd).args(prog).output()?;
        let stderr = String::from_utf8(output.stderr).map_err(|e| anyhow!(e))?;
        let stdout = String::from_utf8(output.stdout).map_err(|e| anyhow!(e))?;
        let code = output
            .status
            .code()
            .map(|c| Value::Int(c as i64))
            .unwrap_or(Value::Unit);

        let m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::from_iter([
            (
                Rc::new("success?".into()),
                Rc::new(output.status.success().into()),
            ),
            (Rc::new("stdout".into()), Rc::new(stdout.into())),
            (Rc::new("stderr".into()), Rc::new(stderr.into())),
            (Rc::new("code".into()), Rc::new(code)),
        ]);
        Ok((Rc::new(Value::Map(m)), env.clone()))
    });

    let mut env = Env::of_builtins(entries);
    for (a, t) in aliases {
        let v = env.get(&Rc::<str>::from(t)).expect("alias target").clone();
        env = env.update(a.into(), v);
    }
    env
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

/// Run `action` against the live `State`. Errors when called from inside a
/// render-phase callback.
fn apply(action: Action) -> Result<(), RuntimeError> {
    if in_render_phase() {
        return Err(RuntimeError::TypeMismatch {
            name: "editor-action".into(),
            expected: "non-mutating call".into(),
            got: "called from a render callback".into(),
        });
    }
    let result = with_editor_mut(|st| st.apply(&[Rc::new(action)]));
    result.map_err(|e| RuntimeError::Other(anyhow!("{e}")))
}

/// Escape `s` so it can be embedded as a rizz string literal.
pub fn quote_for_lisp(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Fire `(notify "<msg>")` against `env`.
fn notify_via_env(msg: &str, env: &Env) {
    let src = format!("(notify {})", quote_for_lisp(msg));
    if let Err(e) = rizz::parse_and_run_with_env(src.as_bytes(), env) {
        with_editor_mut(|st| {
            st.record_message(msg);
            st.record_message(&format!("notify failed: {e}"));
        });
    }
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

fn as_ident_or_str(v: &Rc<Value>, name: &str) -> Result<Rc<str>, RuntimeError> {
    match &**v {
        Value::Ident(s) | Value::Str(s) => Ok(s.clone()),
        _ => Err(RuntimeError::type_mismatch(name, "ident|str", v)),
    }
}

fn wrap_shell_style(input: &str) -> String {
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

fn as_u8(v: &Rc<Value>, name: &str) -> Result<u8, RuntimeError> {
    let n = as_int(v, name)?;
    u8::try_from(n).map_err(|_| RuntimeError::TypeMismatch {
        name: name.into(),
        expected: "0..=255".into(),
        got: n.to_string().into(),
    })
}

fn as_usize(v: &Rc<Value>, name: &str) -> Result<usize, RuntimeError> {
    let n = as_int(v, name)?;
    usize::try_from(n).map_err(|_| RuntimeError::TypeMismatch {
        name: name.into(),
        expected: "0..=usize::MAX".into(),
        got: n.to_string().into(),
    })
}

fn display_from_value(v: &Rc<Value>) -> Result<Option<rizz_core::Display>, RuntimeError> {
    use rizz_core::Display;
    match &**v {
        Value::Unit => Ok(None),
        Value::Str(s) | Value::Ident(s) => Ok(Some(Display::String(s.clone()))),
        Value::Map(m) => {
            let key = |k: &str| Rc::new(Value::Str(k.into()));
            if let Some(t) = m.get(&key("text")) {
                let s = as_str(t, "display.text")?;
                return Ok(Some(Display::String(s)));
            }
            if let Some(n) = m.get(&key("space")) {
                let n = as_usize(n, "display.space")?;
                return Ok(Some(Display::Space(n)));
            }
            Err(RuntimeError::type_mismatch(
                "display",
                "{text: ...} | {space: N}",
                v,
            ))
        }
        _ => Err(RuntimeError::type_mismatch(
            "display",
            "str | {text} | {space} | ()",
            v,
        )),
    }
}

// ---- widget helpers -------------------------------------------------------

fn strkey(s: &str) -> Rc<Value> {
    Rc::new(Value::Str(s.into()))
}

fn value_iter(v: &Rc<Value>) -> Box<dyn Iterator<Item = Rc<Value>> + '_> {
    match &**v {
        Value::Array(xs) => Box::new(xs.iter().cloned().collect::<Vec<_>>().into_iter()),
        Value::Unit => Box::new(std::iter::empty()),
        _ => Box::new(Value::iter(v)),
    }
}

fn widget_line(spans: Vec<Rc<Value>>) -> Rc<Value> {
    let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
    m.insert(strkey("type"), Rc::new(Value::Str("line".into())));
    m.insert(strkey("spans"), Rc::new(Value::Array(spans.into())));
    Rc::new(Value::Map(m))
}

fn widget_set_align(v: Rc<Value>, align: &str) -> Rc<Value> {
    if let Value::Map(m) = &*v {
        let mut m = m.clone();
        m.insert(strkey("align"), Rc::new(Value::Str(align.into())));
        Rc::new(Value::Map(m))
    } else {
        v
    }
}

fn widget_stack(dir: &str, children: &Rc<Value>) -> Rc<Value> {
    let kids: Vector<Rc<Value>> = value_iter(children).collect();
    let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
    m.insert(strkey("type"), Rc::new(Value::Str("stack".into())));
    m.insert(strkey("dir"), Rc::new(Value::Str(dir.into())));
    m.insert(strkey("children"), Rc::new(Value::Array(kids)));
    Rc::new(Value::Map(m))
}

fn widget_constrained(kind: &str, n: i64, m_: i64, child: Rc<Value>) -> Rc<Value> {
    let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
    m.insert(strkey("type"), Rc::new(Value::Str("constrained".into())));
    m.insert(strkey("kind"), Rc::new(Value::Str(kind.into())));
    m.insert(strkey("n"), Rc::new(Value::Int(n)));
    m.insert(strkey("m"), Rc::new(Value::Int(m_)));
    m.insert(strkey("child"), child);
    Rc::new(Value::Map(m))
}

fn parse_mode_ident(v: &Rc<Value>) -> Result<EditingMode, RuntimeError> {
    let s = as_ident(v, "mode")?;
    s.parse().map_err(|_| unknown_variant("mode", &s))
}

fn parse_mode_name(v: &Rc<Value>) -> Result<Rc<str>, RuntimeError> {
    as_ident_or_str(v, "mode")
}

fn parse_mode_layers(v: &Rc<Value>) -> Result<Vec<Rc<str>>, RuntimeError> {
    match &**v {
        Value::Array(items) => items.iter().map(parse_mode_name).collect(),
        _ => Ok(vec![parse_mode_name(v)?]),
    }
}

// ---------------------------------------------------------------------------
// Popup property parsing
// ---------------------------------------------------------------------------

fn parse_popup_options(v: &Rc<Value>, spec: &mut PopupSpec) -> Result<(), RuntimeError> {
    let m = match &**v {
        Value::Unit => return Ok(()),
        Value::Map(m) => m,
        _ => {
            return Err(RuntimeError::type_mismatch(
                "popup-open.options",
                "map | ()",
                v,
            ));
        }
    };
    let key = |k: &str| Rc::new(Value::Str(k.into()));
    if let Some(t) = m.get(&key("text")) {
        spec.initial_text = Some(as_str(t, "popup-open.text")?.to_string());
    }
    if let Some(modes) = m.get(&key("modes")) {
        spec.mode_layers = parse_mode_layers(modes)?;
    } else if let Some(mode) = m.get(&key("mode")) {
        spec.mode_layers = vec![parse_mode_name(mode)?];
    }
    if let Some(bm) = m.get(&key("buffer-mode")) {
        spec.buffer_mode = parse_mode_ident(bm)?;
    }
    if let Some(p) = m.get(&key("placement")) {
        spec.placement = parse_placement(p)?;
    }
    if let Some(sc) = m.get(&key("show-cursor")) {
        spec.show_cursor = sc.is_truthy();
    }
    if let Some(sc) = m.get(&key("wrap-mode")) {
        spec.wrap_mode =
            WrapMode::from_str(&as_ident_or_str(sc, "popup-open.wrap-mode")?).unwrap_or_default();
    }
    if let Some(sc) = m.get(&key("wrap-column")) {
        spec.wrap_column = Some(as_int(sc, "popup-open.wrap-column")?.max(0) as u16);
    }
    if let Some(sc) = m.get(&key("break-indent")) {
        spec.breakindent = sc.is_truthy();
    }
    Ok(())
}

fn parse_placement(v: &Rc<Value>) -> Result<Placement, RuntimeError> {
    match &**v {
        Value::Ident(s) | Value::Str(s) => match s.as_ref() {
            "center" | "centered" => Ok(Placement::default()),
            "full" => Ok(Placement::Full),
            other => Err(unknown_variant("placement", other)),
        },
        Value::Map(m) => {
            let key = |k: &str| Rc::new(Value::Str(k.into()));
            let kind = m
                .get(&key("kind"))
                .map(|k| as_ident_or_str(k, "placement.kind"))
                .transpose()?
                .map(|s| s.to_string())
                .unwrap_or_else(|| "center".to_string());
            match kind.as_str() {
                "center" | "centered" => {
                    let width = m
                        .get(&key("w"))
                        .or_else(|| m.get(&key("width")))
                        .map(parse_dim)
                        .transpose()?
                        .unwrap_or(Dim::Frac(0.6));
                    let height = m
                        .get(&key("h"))
                        .or_else(|| m.get(&key("height")))
                        .map(parse_dim)
                        .transpose()?
                        .unwrap_or(Dim::Frac(0.6));
                    Ok(Placement::Centered { width, height })
                }
                "at" => {
                    let x = m
                        .get(&key("x"))
                        .map(|v| as_int(v, "placement.x"))
                        .transpose()?
                        .unwrap_or(0)
                        .max(0) as u16;
                    let y = m
                        .get(&key("y"))
                        .map(|v| as_int(v, "placement.y"))
                        .transpose()?
                        .unwrap_or(0)
                        .max(0) as u16;
                    let width = m
                        .get(&key("w"))
                        .or_else(|| m.get(&key("width")))
                        .map(parse_dim)
                        .transpose()?
                        .unwrap_or(Dim::Cells(40));
                    let height = m
                        .get(&key("h"))
                        .or_else(|| m.get(&key("height")))
                        .map(parse_dim)
                        .transpose()?
                        .unwrap_or(Dim::Cells(10));
                    Ok(Placement::At {
                        x,
                        y,
                        width,
                        height,
                    })
                }
                "side" => {
                    let side = m.get(&key("side")).ok_or_else(|| {
                        RuntimeError::type_mismatch("placement.side", "ident|str", v)
                    })?;
                    let side = match as_ident_or_str(side, "placement.side")?.as_ref() {
                        "top" => Side::Top,
                        "bottom" => Side::Bottom,
                        "left" => Side::Left,
                        "right" => Side::Right,
                        other => return Err(unknown_variant("placement.side", other)),
                    };
                    let size = m
                        .get(&key("size"))
                        .map(parse_dim)
                        .transpose()?
                        .unwrap_or(Dim::Cells(10));
                    Ok(Placement::Anchored { side, size })
                }
                "full" => Ok(Placement::Full),
                other => Err(unknown_variant("placement.kind", other)),
            }
        }
        _ => Err(RuntimeError::type_mismatch("placement", "ident|str|map", v)),
    }
}

fn parse_dim(v: &Rc<Value>) -> Result<Dim, RuntimeError> {
    match &**v {
        Value::Int(n) => Ok(Dim::Cells((*n).max(0) as u16)),
        Value::Float(f) => Ok(Dim::Frac(f.into_inner() as f32)),
        _ => Err(RuntimeError::type_mismatch("dim", "int|float", v)),
    }
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
        let v = s.eval_lisp(r#"(span "hi" 'header)"#).unwrap();
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
        s.eval_lisp(r#"(fn _star () (text "★" ()))"#).unwrap();
        s.eval_lisp(r#"(set-frame _star)"#).unwrap();
        let (_, err) = s.precompute_frame();
        assert!(err.is_none(), "no frame errors: {err:?}");
    }

    #[test]
    fn default_style_lisp_loads_clean() {
        let mut s = test_state();
        let src = include_str!("../../../default-style.rz");
        s.eval_lisp_script(src)
            .map_err(|e| format!("default-style.lisp eval failed: {e}"))
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
}
