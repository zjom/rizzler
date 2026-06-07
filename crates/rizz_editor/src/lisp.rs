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
        Self::with_env(env)
    }
    pub fn with_env(env: Env) -> Self {
        Self(Runtime::with_env(env))
    }

    pub fn set_basedir(&mut self, p: impl Into<PathBuf>) {
        self.0 = Runtime::with_env(self.0.env().clone().with_base_dir(Some(p.into())))
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

// ---------------------------------------------------------------------------
// Builtin registration
// ---------------------------------------------------------------------------

fn builtins() -> Env {
    let mut entries: Vec<(&str, NativeFn)> = Vec::new();
    let mut aliases: Vec<(&str, &str)> = Vec::new();
    // Default builtin: reads env (for `with_editor_mut` callbacks that need
    // theme/state lookups, or just to thread sibling bindings) but does not
    // extend it. The closure returns just the value.
    macro_rules! be {
        ($name:expr, $nargs:expr, $f:expr) => {
            entries.push(($name, NativeFn::with_env($name.into(), $nargs, $f)));
        };
        ($name:expr, $nargs:expr, $f:expr, $doc:expr) => {
            entries.push((
                $name,
                NativeFn::with_env($name.into(), $nargs, $f).with_doc(Rc::from($doc)),
            ));
        };
    }
    // Env-extending builtin. The closure returns `(value, new_env)` and the
    // returned env is threaded back into the caller's scope by the runtime —
    // use this only for forms that genuinely introduce top-level bindings.
    macro_rules! bi {
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
    be!(
        "quit",
        0,
        |_, _| {
            apply(Action::Quit)?;
            Ok(unit())
        },
        "(quit/0)\nexit the application"
    );
    alias!("q" => "quit");

    be!(
        "set-mode",
        1,
        |args, _| {
            let mode = parse_mode_ident(&args[0])?;
            apply(Action::SetMode(mode))?;
            Ok(unit())
        },
        "(set-mode/1)\nchange the editing mode.\naccepts one of: 'normal | 'insert | 'visual | 'visual-line | 'visual-block | 'command"
    );

    // text editing
    be!("insert-char", 1, |args, _| {
        let s = as_str(&args[0], "insert-char")?;
        let c = s
            .chars()
            .next()
            .ok_or_else(|| str_mismatch("insert-char", "non-empty str"))?;
        apply(Action::InsertChar(c))?;
        Ok(unit())
    });
    be!("insert", 1, |args, _| {
        let s = as_str(&args[0], "insert")?;
        apply(Action::InsertMany(s))?;
        Ok(unit())
    });
    be!("delete-char", 0, |_, _| {
        apply(Action::DeleteChar)?;
        Ok(unit())
    });

    be!("delete-char-at", 2, |args, _| {
        let col = as_usize(&args[0], "delete-char-at")?;
        let row = as_usize(&args[1], "delete-char-at")?;
        apply(Action::DeleteCharAt(Position::new(col, row)))?;
        Ok(unit())
    });
    be!("delete-selection", 0, |_, _| {
        apply(Action::DeleteSelection)?;
        Ok(unit())
    });
    be!("delete-line", 0, |_, _| {
        let count = with_editor_mut(|st| st.pending_count_or_one());
        apply(Action::DeleteLine { count })?;
        Ok(unit())
    });
    be!("delete-motion", 1, |args, _| {
        let sym = as_ident(&args[0], "delete-motion")?;
        let kind = MoveKind::from_str(&sym).map_err(|_| unknown_variant("delete-motion", &sym))?;
        let count = with_editor_mut(|st| st.pending_count_or_one());
        apply(Action::DeleteMotion { kind, count })?;
        Ok(unit())
    });
    be!("newline", 0, |_, _| {
        apply(Action::InsertNewline)?;
        Ok(unit())
    });
    be!("undo", 0, |_, _| {
        apply(Action::Undo)?;
        Ok(unit())
    });
    be!("redo", 0, |_, _| {
        apply(Action::Redo)?;
        Ok(unit())
    });

    // cursor movement
    be!("move-cursor", 1, |args, _| {
        let sym = as_ident(&args[0], "move-cursor")?;
        let mk = MoveKind::from_str(&sym).map_err(|_| unknown_variant("move-cursor", &sym))?;
        let count = with_editor_mut(|st| st.pending_count_or_one());
        apply(Action::MoveCursor { kind: mk, count })?;
        Ok(unit())
    });
    be!("move-cursor-rel", 2, |args, _| {
        let dx = as_int(&args[0], "move-cursor-rel")?;
        let dy = as_int(&args[1], "move-cursor-rel")?;
        let mk = MoveKind::Relative(Position::new(dx as i16, dy as i16));
        let count = with_editor_mut(|st| st.pending_count_or_one());
        apply(Action::MoveCursor { kind: mk, count })?;
        Ok(unit())
    });
    be!("line", 1, |args, _| {
        let n = as_int(&args[0], "line")?;
        let mk = MoveKind::LineNum(n.max(0) as usize);
        apply(Action::MoveCursor { kind: mk, count: 1 })?;
        Ok(unit())
    });

    // buffer management
    be!("buf-create", 0, |_, _| {
        apply(Action::BufCreate {
            set_active: true,
            path: None,
        })?;
        Ok(unit())
    });
    alias!("bc" => "buf-create");
    be!("buf-delete", 0, |_, _| {
        apply(Action::BufDelete)?;
        Ok(unit())
    });
    alias!("bd" => "buf-delete");
    be!("buf-next", 0, |_, _| {
        apply(Action::BufNext)?;
        Ok(unit())
    });
    alias!("bn" => "buf-next");
    be!("buf-prev", 0, |_, _| {
        apply(Action::BufPrev)?;
        Ok(unit())
    });
    alias!("bp" => "buf-prev");
    be!("edit", 1, |args, _| {
        let p = as_str(&args[0], "edit")?;
        let path = std::path::PathBuf::from_str(&p).unwrap();
        apply(Action::BufEdit(path.into()))?;
        Ok(unit())
    });
    alias!("e" => "edit");
    be!("write", 0, |_, _| {
        apply(Action::BufWrite(None))?;
        Ok(unit())
    });
    alias!("w" => "write");
    be!("write-as", 1, |args, _| {
        let p = as_str(&args[0], "write-as")?;
        let path = std::path::PathBuf::from_str(&p).unwrap();
        apply(Action::BufWrite(Some(path.into())))?;
        Ok(unit())
    });

    // windows
    be!("window-split", 1, |args, _| {
        let dir = match as_ident(&args[0], "window-split")?.as_ref() {
            "vertical" | "v" => SplitDir::Vertical,
            "horizontal" | "h" => SplitDir::Horizontal,
            other => return Err(unknown_variant("window-split", other)),
        };
        apply(Action::WindowSplit(dir))?;
        Ok(unit())
    });
    be!("window-close", 0, |_, _| {
        apply(Action::WindowClose)?;
        Ok(unit())
    });
    be!("window-focus", 1, |args, _| {
        let dir = match as_ident(&args[0], "window-focus")?.as_ref() {
            "left" => FocusDir::Left,
            "right" => FocusDir::Right,
            "up" => FocusDir::Up,
            "down" => FocusDir::Down,
            other => return Err(unknown_variant("window-focus", other)),
        };
        apply(Action::WindowFocus(dir))?;
        Ok(unit())
    });
    be!("window-focus-next", 0, |_, _| {
        apply(Action::WindowFocusNext)?;
        Ok(unit())
    });

    // keymap
    be!("keymap-set", 3, |args, _| {
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
        Ok(unit())
    });

    be!("keymap-remove", 2, |args, _| {
        let mode = parse_mode_name(&args[0])?;
        let lhs_str = as_str(&args[1], "keymap-remove")?;
        let lhs = KeyEvent::parse_sequence(&lhs_str)
            .map_err(|e| str_mismatch_msg("keymap-remove", &e))?;
        apply(Action::KeymapRemove { mode, lhs })?;
        Ok(unit())
    });

    be!("keymap-get", 1, |args, _| {
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
        Ok(Rc::new(mappings.into()))
    });

    // minibuffer flow
    bi!("command-submit", 0, |_, env| {
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
                Ok((unit(), env.clone()))
            }
        }
    });

    be!("command-cancel", 0, |_, _| {
        apply(Action::CommandCancel)?;
        Ok(unit())
    });

    bi!("evaluate", 0, |_, env| {
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
                Ok((unit(), env.clone()))
            }
        }
    });

    be!("notify-record", 1, |args, _| {
        let s = as_str(&args[0], "notify-record")?;
        with_editor_mut(|st| st.record_message(&s));
        Ok(unit())
    });
    be!("message-history", 0, |_, _| {
        let msgs: Vector<Rc<Value>> = with_editor_mut(|st| {
            st.message_history()
                .map(|s| Rc::new(Value::Str(s.clone())))
                .collect()
        });
        Ok(Rc::new(Value::Array(msgs)))
    });
    be!("command-history", 0, |_, _| {
        let cmds: Vector<Rc<Value>> = with_editor_mut(|st| {
            st.cmd_history()
                .map(|s| Rc::new(Value::Str(s.clone())))
                .collect()
        });
        Ok(Rc::new(Value::Array(cmds)))
    });

    // popups
    be!("popup-open", 1, |args, _| {
        let widget = with_editor_mut(|st| {
            let theme = st.theme().borrow();
            parse_widget(&args[0], &theme)
        })?;
        let mut spec = PopupSpec::new(widget);
        if let Some(opts) = args.get(1) {
            parse_popup_options(opts, &mut spec)?;
        }
        let bufno = with_editor_mut(|st| st.open_popup(spec));
        Ok(Rc::new(Value::Int(bufno as i64)))
    });
    be!("popup-close", 0, |_, _| {
        let closed = with_editor_mut(|st| st.close_popup());
        Ok(Rc::new(Value::Int(closed as i64)))
    });
    be!("popup-bufno", 0, |_, _| {
        let v = with_editor_mut(|st| {
            st.top_popup_bufno()
                .map(|n| Value::Int(n as i64))
                .unwrap_or(Value::Unit)
        });
        Ok(Rc::new(v))
    });
    be!("minibuffer-bufno", 0, |_, _| {
        let n = with_editor_mut(|st| st.minibuffer_bufno());
        Ok(Rc::new(Value::Int(n as i64)))
    });
    be!("popup-mode", 0, |_, _| {
        let v = with_editor_mut(|st| st.top_popup_mode().map(Value::Str).unwrap_or(Value::Unit));
        Ok(Rc::new(v))
    });
    be!("popup?", 0, |_, _| {
        let v = with_editor_mut(|st| st.has_popup());
        Ok(Rc::new(Value::Int(v as i64)))
    });

    // queries
    be!("buf-text-set", 2, |args, _| {
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
        Ok(unit())
    });

    be!("buf-text", 0, |_, _| {
        let s = with_editor_mut(|st| st.focused_buf().text());
        Ok(Rc::new(s.into()))
    });

    be!("buf-no", 0, |_, _| {
        let s = with_editor_mut(|st| st.focused_bufno());
        Ok(Rc::new(Value::Int(s as i64)))
    });

    be!("buf-path", 0, |_, _| {
        let v: Value = with_editor_mut(|st| st.focused_buf().fs_path())
            .map(|p| p.to_string_lossy().as_ref().into())
            .map(|s: Rc<str>| Value::Str(s))
            .unwrap_or(Value::Unit);
        Ok(Rc::new(v))
    });
    alias!("%"=>"buf-path");

    be!("selected-text", 0, |_, _| {
        let s = with_editor_mut(|st| st.focused_buf().selected_text());
        Ok(Rc::new(s.into()))
    });

    be!("cursor-line", 0, |_, _| {
        let n = with_editor_mut(|st| st.focused_buf().abs_row() as i64);
        Ok(Rc::new(n.into()))
    });

    be!("line-at", 1, |args, _| {
        let idx = as_usize(&args[0], "line-at")?;
        let s = with_editor_mut(|st| st.focused_buf().lines_at(idx).next().map(|s| s.to_string()));
        Ok(Rc::new(s.into()))
    });

    be!("cursor-col", 0, |_, _| {
        let n = with_editor_mut(|st| st.focused_buf().abs_col() as i64);
        Ok(Rc::new(n.into()))
    });

    // wrap settings
    be!("buffer-wrap", 0, |args, _| {
        if let Some(arg) = args.first() {
            let sym = as_ident_or_str(arg, "buffer-wrap")?;
            let m = WrapMode::from_str(&sym).ok_or_else(|| unknown_variant("buffer-wrap", &sym))?;
            with_editor_mut(|st| st.focused_buf_mut().set_wrap_mode(m));
            Ok(unit())
        } else {
            let s: Rc<str> = with_editor_mut(|st| st.focused_buf().wrap_mode().as_str().into());
            Ok(Rc::new(Value::Str(s)))
        }
    });
    be!("buffer-wrap?", 0, |_, _| {
        let s: Rc<str> = with_editor_mut(|st| st.focused_buf().wrap_mode().as_str().into());
        Ok(Rc::new(Value::Str(s)))
    });

    be!("buffer-wrap-column", 1, |args, _| {
        let n = as_int(&args[0], "buffer-wrap-column")?;
        let col = if n <= 0 {
            None
        } else {
            Some(n.min(u16::MAX as i64) as u16)
        };
        with_editor_mut(|st| st.focused_buf_mut().set_wrap_column(col));
        Ok(unit())
    });

    be!("buffer-breakindent", 1, |args, _| {
        let n = as_int(&args[0], "buffer-breakindent")?;
        with_editor_mut(|st| st.focused_buf_mut().set_breakindent(n != 0));
        Ok(unit())
    });

    // styling: faces + colors
    be!("face-define", 2, |args, _| {
        let name = as_ident_or_str(&args[0], "face-define")?;
        let style = with_editor_mut(|st| {
            let theme = st.theme().borrow();
            style_from_value(&args[1], &theme)
        })?;
        with_editor_mut(|st| {
            st.theme().borrow_mut().insert(name, style);
        });
        Ok(unit())
    });
    be!("face-of", 1, |args, _| {
        let name = as_ident_or_str(&args[0], "face-of")?;
        let v = with_editor_mut(|st| {
            st.theme()
                .borrow()
                .lookup(&name)
                .map(style_to_value)
                .unwrap_or_else(|| Rc::new(Value::Unit))
        });
        Ok(v)
    });
    be!("rgb", 3, |args, _| {
        let r = as_u8(&args[0], "rgb")?;
        let g = as_u8(&args[1], "rgb")?;
        let b = as_u8(&args[2], "rgb")?;
        Ok(rgb_value(r, g, b))
    });
    be!("span", 2, |args, _| {
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
        Ok(Rc::new(Value::Map(m)))
    });

    // ---- widget tree builtins ---------------------------------------------

    be!("set-frame", 1, |args, _| {
        let v = args[0].clone();
        let opt = if v.is_unit() { None } else { Some(v) };
        with_editor_mut(|st| st.set_frame_fn(opt));
        Ok(unit())
    });

    be!("text", 2, |args, _| {
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
        Ok(Rc::new(Value::Map(span)))
    });

    be!("line", 1, |args, _| {
        let spans: Vec<Rc<Value>> = value_iter(&args[0]).collect();
        Ok(widget_line(spans))
    });

    be!("right-align", 1, |args, _| {
        Ok(widget_set_align(args[0].clone(), "right"))
    });
    be!("center-align", 1, |args, _| {
        Ok(widget_set_align(args[0].clone(), "center"))
    });

    be!("vstack", 1, |args, _| {
        Ok(widget_stack("vertical", &args[0]))
    });
    be!("hstack", 1, |args, _| {
        Ok(widget_stack("horizontal", &args[0]))
    });

    be!("cells", 2, |args, _| {
        let n = as_int(&args[0], "cells")?.max(0).min(u16::MAX as i64);
        Ok(widget_constrained("cells", n, 1, args[1].clone()))
    });
    be!("min-cells", 2, |args, _| {
        let n = as_int(&args[0], "min-cells")?.max(0).min(u16::MAX as i64);
        Ok(widget_constrained("min", n, 1, args[1].clone()))
    });
    be!("fill", 2, |args, _| {
        let n = as_int(&args[0], "fill")?.max(0).min(u16::MAX as i64);
        Ok(widget_constrained("fill", n, 1, args[1].clone()))
    });
    be!("frac", 3, |args, _| {
        let n = as_int(&args[0], "frac")?.max(0).min(u16::MAX as i64);
        let m = as_int(&args[1], "frac")?.max(1).min(u16::MAX as i64);
        Ok(widget_constrained("frac", n, m, args[2].clone()))
    });

    be!("block", 2, |args, _| {
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
        Ok(Rc::new(Value::Map(m)))
    });

    be!("editor-tree", 1, |args, _| {
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
        Ok(Rc::new(Value::Map(m)))
    });

    be!("minibuffer", 0, |_, _| {
        let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
        m.insert(strkey("type"), Rc::new(Value::Str("minibuffer".into())));
        Ok(Rc::new(Value::Map(m)))
    });

    be!("empty", 0, |_, _| {
        let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
        m.insert(strkey("type"), Rc::new(Value::Str("empty".into())));
        Ok(Rc::new(Value::Map(m)))
    });

    be!("buffer-view", 0, |args, _| {
        let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
        m.insert(strkey("type"), Rc::new(Value::Str("buffer-view".into())));
        if let Some(arg) = args.first() {
            let bufno = as_int(arg, "buffer-view.bufno")?.max(0);
            m.insert(strkey("bufno"), Rc::new(Value::Int(bufno)));
        }
        Ok(Rc::new(Value::Map(m)))
    });

    // text properties + overlays
    be!("put-text-property", 5, |args, _| {
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
        Ok(unit())
    });
    be!("clear-text-properties", 0, |_, _| {
        with_editor_mut(|st| {
            st.focused_buf_mut().props_mut().clear_text_properties();
        });
        Ok(unit())
    });

    be!("overlay-create", 5, |args, _| {
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
        Ok(Rc::new(Value::Int(id.0 as i64)))
    });
    be!("overlay-put", 3, |args, _| {
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
        Ok(unit())
    });
    be!("overlay-delete", 1, |args, _| {
        let id = rizz_text::OverlayId(as_int(&args[0], "overlay-delete")? as u64);
        with_editor_mut(|st| {
            st.focused_buf_mut().props_mut().delete_overlay(id);
        });
        Ok(unit())
    });

    be!("focused-mode", 0, |_, _| {
        let s = with_editor_mut(|st| st.focused_buf().mode().as_str());
        Ok(Rc::new(Value::Str(s.into())))
    });

    be!("last-key", 0, |_, _| {
        let s = with_editor_mut(|st| {
            st.last_key()
                .map(|k| k.code.to_string())
                .unwrap_or_else(|| "None".to_string())
        });
        Ok(Rc::new(Value::Str(s.into())))
    });

    be!("workdir", 0, |_, _| {
        let d: Value = with_editor_mut(|st| st.workdir()).as_ref().into();
        Ok(Rc::new(d))
    });

    be!(
        "config-dir",
        0,
        |_, _| {
            let d: Value = with_editor_mut(|st| st.config_dir()).as_ref().into();
            Ok(Rc::new(d))
        },
        "(config-dir/0)\nreturn the directory holding init.rz"
    );

    bi!(
        "reload-config",
        0,
        |_, env| {
            // Read source + capture config dir under the editor borrow, then
            // drop it before eval so the parser can re-enter editor builtins.
            let (src, dir) =
                with_editor_mut(|st| st.load_init_script().map(|src| (src, st.config_dir())))
                    .map_err(|e| RuntimeError::Other(anyhow!("{e}")))?;
            let prev_basedir = env.base_dir().map(PathBuf::from);
            let eval_env = env.clone().with_base_dir(Some(dir.as_ref().to_path_buf()));
            let (_, new_env) = rizz::parse_and_run_with_env(src.as_bytes(), &eval_env)
                .map_err(|e| RuntimeError::Other(anyhow!("{e}")))?;
            Ok((unit(), new_env.with_base_dir(prev_basedir)))
        },
        "(reload-config/0)\nre-read init.rz from the config dir and evaluate it"
    );

    be!("fs-canonicalize", 1, |args, _| {
        let s = as_str(&args[0], "fs-canonicalize")?;
        let path = std::fs::canonicalize(s.as_ref())?;
        Ok(Rc::new(path.into()))
    });

    be!("fs-parent", 1, |args, _| {
        let s = as_str(&args[0], "fs-parent")?;
        let path = PathBuf::from_str(&s).unwrap();
        if let Some(parent) = path.parent()
            && parent.exists()
        {
            Ok(Rc::new(parent.into()))
        } else {
            Ok(unit())
        }
    });

    be!("fs-readdir", 1, |args, _| {
        let path = as_str(&args[0], "fs-readdir")?;
        let dirs = std::fs::read_dir(path.as_ref())?
            .map(|res| res.map(|e| e.path().into()))
            .collect::<Result<Vector<Value>, std::io::Error>>()?;
        Ok(Rc::new(dirs.into()))
    });
    alias!("ls"=>"fs-readdir");
    alias!("readdir"=>"fs-readdir");

    be!("fs-isdir", 1, |args, _| {
        let path = as_str(&args[0], "fs-isdir")?;
        let meta = std::fs::metadata(path.as_ref())?;
        Ok(Rc::new(meta.is_dir().into()))
    });

    be!("exec", 1, |args, _| {
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
        Ok(Rc::new(Value::Map(m)))
    });

    // Runtime-loaded tree-sitter grammars. `(grammar-register name lib-path
    // scm-path ext)` opens the shared library, resolves its
    // `tree_sitter_<name>` factory, compiles the highlights query, and
    // indexes it by `ext` — either a single string like `".py"` or an array
    // of strings.
    be!(
        "grammar-register",
        4,
        |args, _| {
            let name = as_str(&args[0], "grammar-register")?;
            let lib_path = as_str(&args[1], "grammar-register")?;
            let scm_path = as_str(&args[2], "grammar-register")?;
            let exts = parse_extensions(&args[3])?;
            let highlights = std::fs::read_to_string(scm_path.as_ref())?;
            let lib_path = std::path::PathBuf::from(lib_path.as_ref());
            with_editor_mut(|st| st.register_grammar(&name, &exts, &lib_path, &highlights))
                .map_err(|e| RuntimeError::Other(anyhow!("{e}")))?;
            Ok(unit())
        },
        "(grammar-register/4)\nregister a tree-sitter grammar loaded from a shared library (.so/.dylib/.dll).\nthe library must export `tree_sitter_<name>` — Neovim's `parser/*.so` ABI.\nargs: <name str> <library-path str> <highlights.scm path str> <ext: str | [str ...]>"
    );

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

/// Accept either a single extension string (`".py"`, `"py"`) or an array of
/// such strings. The leading dot is optional in both cases.
fn parse_extensions(v: &Rc<Value>) -> Result<Vec<String>, RuntimeError> {
    match &**v {
        Value::Array(items) => items
            .iter()
            .map(|x| as_str(x, "grammar-register.ext").map(|s| s.to_string()))
            .collect(),
        _ => Ok(vec![as_str(v, "grammar-register.ext")?.to_string()]),
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
                    // Default to Fit: the popup sizes itself to the minimum
                    // rows/cols needed to contain the buffer's wrapped text.
                    let size = m
                        .get(&key("size"))
                        .map(parse_dim)
                        .transpose()?
                        .unwrap_or(Dim::Fit);
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
        Value::Ident(s) | Value::Str(s) if s.as_ref() == "fit" => Ok(Dim::Fit),
        _ => Err(RuntimeError::type_mismatch("dim", "int|float|'fit", v)),
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
        let src = include_str!("../../../init.rz");
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
