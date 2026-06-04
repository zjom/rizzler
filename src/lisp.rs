//! Embedded lisp runtime (rizz) bridged to the editor.
//!
//! The runtime owns a persistent [`Env`] of bindings — including every editor
//! primitive exposed as a native function — and is threaded through one entry
//! point on `State` (`State::eval_lisp` / `eval_lisp_value`). Editor primitives
//! access mutable `State` via an RAII-guarded thread-local pointer (see
//! [`with_editor_mut`]): the runtime is moved out of `State` for the duration
//! of an eval, so there is never simultaneous aliasing of `&mut State`.

use std::cell::Cell;
use std::path::PathBuf;
use std::process;
use std::ptr::NonNull;
use std::rc::Rc;
use std::str::FromStr;

use anyhow::anyhow;
use im::{HashMap as ImHashMap, Vector};
use rizz::RizzError;
use rizz::runtime::{self, Env, NativeFn, RuntimeError, Value};

use crate::action::Action;
use crate::buffer::MoveKind;
use crate::keymap::KeyEvent;
use crate::mode::EditingMode;
use crate::popup::{BorderStyle, Dim, Placement, Side};
use crate::position::Position;
use crate::regions::{BuiltinId, Producer, Region, RegionAnchor};
use crate::state::{PopupSpec, State};
use crate::styling::{normalize_style_value, rgb_value, style_from_value, style_to_value};
use crate::window::{FocusDir, SplitDir};
use crate::wrap::WrapMode;

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
/// precompute pass. Constructed inside [`State::precompute_frame`].
pub(crate) struct RenderPhaseGuard;

impl RenderPhaseGuard {
    pub(crate) fn enter() -> Self {
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
        let env = rizz::prelude::install(builtins());
        Self { env }
    }

    /// Parse `src` as one top-level form, evaluate it, and update `self.env`
    /// with any new bindings the form introduced.
    pub fn eval_str(&mut self, src: &str) -> Result<Rc<Value>, RizzError> {
        let (v, env) = rizz::parse_and_run_with_env(src.as_bytes(), &self.env)?;
        self.env = env;
        Ok(v)
    }

    /// Evaluate an already-parsed form. Used by `Action::EvalLisp` so that
    /// keybindings don't re-parse on every keystroke.
    pub fn eval_value(&mut self, form: Rc<Value>) -> Result<Rc<Value>, RizzError> {
        let (v, env) = runtime::eval(form, &self.env)?;
        self.env = env;
        Ok(v)
    }

    /// Evaluate a multi-form script: `;`-introduced line comments are stripped
    /// and each top-level form is parsed and evaluated in sequence. Stops on
    /// the first error and returns it.
    pub fn eval_script(&mut self, src: &str) -> Result<(), RizzError> {
        self.eval_str(src)?;
        Ok(())
    }

    /// Borrow the current environment. Used by the precompute pass to feed
    /// render callbacks (`runtime::apply`) against the live bindings.
    pub fn env(&self) -> &Env {
        &self.env
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
// Builtin registration
// ---------------------------------------------------------------------------

fn builtins() -> Env {
    let mut entries: Vec<(&str, NativeFn)> = Vec::new();
    let mut aliases: Vec<(&str, &str)> = Vec::new();
    macro_rules! b {
        ($name:expr, $nargs:expr, $f:expr) => {
            entries.push(($name, NativeFn::impure($name.into(), $nargs, $f)));
        };
    }
    macro_rules! alias {
        ($a:expr => $t:expr) => {
            aliases.push(($a, $t));
        };
    }

    // mode + lifecycle
    b!("quit", 0, |_, env| {
        apply(Action::Quit)?;
        ok_unit(env)
    });
    alias!("q" => "quit");

    b!("set-mode", 1, |args, env| {
        let mode = parse_mode_ident(&args[0])?;
        apply(Action::SetMode(mode))?;
        ok_unit(env)
    });

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
    b!("newline", 0, |_, env| {
        apply(Action::InsertNewline)?;
        ok_unit(env)
    });

    // cursor movement
    b!("move-cursor", 1, |args, env| {
        let sym = as_ident(&args[0], "move-cursor")?;
        let mk = MoveKind::from_str(&sym).map_err(|_| unknown_variant("move-cursor", &sym))?;

        apply(Action::MoveCursor(mk))?;
        ok_unit(env)
    });
    b!("move-cursor-rel", 2, |args, env| {
        let dx = as_int(&args[0], "move-cursor-rel")?;
        let dy = as_int(&args[1], "move-cursor-rel")?;
        let mk = MoveKind::Relative(Position::new(dx as i16, dy as i16));
        apply(Action::MoveCursor(mk))?;
        ok_unit(env)
    });
    b!("line", 1, |args, env| {
        let n = as_int(&args[0], "line")?;
        let mk = MoveKind::LineNum(n.max(0) as usize);
        apply(Action::MoveCursor(mk))?;
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

    // keymap (lisp owns the keymap; lhs is parsed via `KeyEvent::parse_sequence`).
    //
    // `mode` is a free-form string here, not an `EditingMode`. That's what lets
    // user-defined popup modes (`'popup`, `'popup.files`, …) bind keys the
    // same way as `'normal` / `'insert` do.
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

    // Returns the bindings registered against `mode` as an array of maps:
    //
    //     [{"mode": "normal" "lhs": "<c-w>q" "rhs": (window-close)} ...]
    //
    // `lhs` is a key-sequence string that round-trips through
    // `KeyEvent::parse_sequence`, and `rhs` is the lisp form the binding
    // was originally created with (or a `Debug` string for non-lisp
    // actions, e.g. the seeded `on_char` typing handlers). This lets lisp
    // code introspect bindings — popups can copy or reuse them instead of
    // redefining every key from scratch.
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
    //
    // `command-submit` runs inside an `eval_lisp_value` frame (it's fired from
    // the `<enter>` keybinding in command mode), so it cannot call back into
    // `State::eval_lisp` — that would re-take `self.lisp` and panic. Parse and
    // evaluate the user's input directly against `env` instead.
    b!("command-submit", 0, |_, env| {
        let cmd = with_editor_mut(|st| st.take_minibuffer_command());
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
        let src = with_editor_mut(|st| {
            st.focused_buf()
                .selected_text()
                .unwrap_or_else(|| st.focused_buf().text())
        });
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
    // Append a message to the history. The user-facing `notify` fn is
    // defined in `default.lisp` on top of this primitive + `popup-open`,
    // so popup styling, dedup, and dismissal all live in lisp.
    b!("notify-record", 1, |args, env| {
        let s = as_str(&args[0], "notify-record")?;
        with_editor_mut(|st| st.record_message(&s));
        ok_unit(env)
    });
    b!("message-history", 0, |_, env| {
        let msgs: Vector<Rc<Value>> = with_editor_mut(|st| {
            st.message_history()
                .iter()
                .map(|s| Rc::new(Value::Str(s.clone())))
                .collect()
        });
        Ok((Rc::new(Value::Array(msgs)), env.clone()))
    });

    // popups — generalized overlay surface. A popup is conceptually a buffer
    // drawn on top of the editor area with chrome (border/title) and a
    // keymap mode that captures input while the popup is on top of the
    // stack. The message popup is a thin preset on top of these; user
    // popups (file picker, terminal, prompt, …) plug in placement, chrome,
    // and keymap mode to express whatever shape they want.
    //
    // (popup-open props) — `props` is a map (see `parse_popup_props`).
    //                     Returns the popup buffer's bufno.
    // (popup-close)      — closes the topmost popup. Returns 1 / 0.
    // (popup-bufno)      — bufno of the topmost popup, or `()`.
    // (popup?)           — 1 if any popup is open, else 0.
    b!("popup-open", 1, |args, env| {
        let spec = parse_popup_props(&args[0])?;
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
    // Keymap mode of the topmost popup as a string, or `()` if no popup is
    // open. Lets lisp-side popup managers (like `notify` in default.lisp)
    // dedup against their own popup instead of stacking new ones.
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
        // `display` keeps strings raw (no surrounding quotes) while still
        // formatting non-strings sensibly — that's what callers expect when
        // setting a buffer's text from a plain string value.
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

    // Soft-wrap, per buffer. Pass an argument to set; call with no arg to read.
    // `mode` is one of `'none` / `'char` / `'word` (or the strings of same).
    b!("buffer-wrap", 1, |args, env| {
        let sym = as_ident_or_str(&args[0], "buffer-wrap")?;
        let m = crate::wrap::WrapMode::from_str(&sym)
            .ok_or_else(|| unknown_variant("buffer-wrap", &sym))?;
        with_editor_mut(|st| st.focused_buf_mut().set_wrap_mode(m));
        ok_unit(env)
    });
    b!("buffer-wrap?", 0, |_, env| {
        let s: Rc<str> = with_editor_mut(|st| st.focused_buf().wrap_mode().as_str().into());
        Ok((Rc::new(Value::Str(s)), env.clone()))
    });

    // Fixed wrap column. Pass 0 (or negative) to clear, meaning "wrap to
    // the content area width".
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

    // breakindent: pass non-zero to enable, 0 to disable.
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
    // span constructor used by render slots. Two forms:
    //   (span "text")              -> {text: "text"}
    //   (span "text" face-or-map)  -> {text: "text" style: ...}
    // We don't resolve the style here — slot precompute does, holding a
    // theme snapshot.
    b!("span", 2, |args, env| {
        use im::HashMap as ImHashMap;
        let text = as_str(&args[0], "span")?;
        // Normalize the style argument before storing — rizz re-evaluates a
        // native fn's return, so any Ident still in the returned map would
        // be resolved as a variable lookup.
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

    // region registration. The anchor argument decides where the region
    // appears on screen; the handler is one of:
    //   * ident → resolved via `BuiltinId::parse` (must match the anchor)
    //   * callable → invoked each frame
    //   * any other value → stored as a Static lisp value
    //
    // Anchor syntax (see `parse_anchor`):
    //   'status-left | 'status-right | 'decorator
    //   'top         | 'bottom              (full-width strip)
    //   {"gutter": N}                       (left column, N wide)
    //
    // Names are globally unique across all anchors — re-adding by the same
    // name replaces the previous registration in place.
    b!("region-add", 3, |args, env| {
        let name = as_ident_or_str(&args[0], "region-add")?;
        let anchor = parse_anchor(&args[1])?;
        let producer = parse_handler(&args[2], "region-add", &anchor)?;
        with_editor_mut(|st| {
            st.regions_mut().add(Region {
                name,
                anchor,
                producer,
            });
        });
        ok_unit(env)
    });
    b!("region-remove", 1, |args, env| {
        let name = as_ident_or_str(&args[0], "region-remove")?;
        with_editor_mut(|st| {
            st.regions_mut().remove(&name);
        });
        ok_unit(env)
    });

    // text properties + overlays. Both attach a style range to the focused
    // buffer. `put-text-property` adds an anonymous entry; `overlay-create`
    // returns a handle (int) so the entry can be mutated/removed later. The
    // style argument accepts whatever `style_from_value` does — a face name
    // or an inline `{"fg": ... "bg": ...}` map.
    //
    // Ranges are half-open `[start, end)`. Multi-row spans are clipped to
    // each visible row at render time.
    b!("put-text-property", 5, |args, env| {
        let start_row = as_usize(&args[0], "put-text-property")?;
        let start_col = as_usize(&args[1], "put-text-property")?;
        let end_row = as_usize(&args[2], "put-text-property")?;
        let end_col = as_usize(&args[3], "put-text-property")?;
        let face = args[4].clone();
        with_editor_mut(|st| {
            st.focused_buf_mut()
                .props_mut()
                .push_text_property(crate::props::PropEntry {
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
            st.focused_buf_mut()
                .props_mut()
                .create_overlay(crate::props::PropEntry {
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
    // `(overlay-put id key value)` — currently recognized keys:
    //   "face"         — face name or inline style map
    //   "priority"     — int; higher wins among overlapping overlays
    //   "pad-to-width" — truthy/falsy; pad the highlight to the area width
    //   "display"      — visual substitution; one of:
    //                      a string                        → replace with text
    //                      {"text": "..."}                 → same as above
    //                      {"space": N}                    → replace with N spaces
    //                      ()                              → clear any display
    b!("overlay-put", 3, |args, env| {
        let id = crate::props::OverlayId(as_int(&args[0], "overlay-put")? as u64);
        let key = as_ident_or_str(&args[1], "overlay-put")?;
        match key.as_ref() {
            "face" => {
                let face = args[2].clone();
                with_editor_mut(|st| {
                    if let Some(e) = st.focused_buf_mut().props_mut().overlay_mut(id) {
                        e.face = Some(face);
                    }
                });
            }
            "priority" => {
                let p = as_int(&args[2], "overlay-put")?;
                with_editor_mut(|st| {
                    if let Some(e) = st.focused_buf_mut().props_mut().overlay_mut(id) {
                        e.priority = p;
                    }
                });
            }
            "pad-to-width" => {
                let pad = args[2].is_truthy();
                with_editor_mut(|st| {
                    if let Some(e) = st.focused_buf_mut().props_mut().overlay_mut(id) {
                        e.pad_to_width = pad;
                    }
                });
            }
            "display" => {
                let disp = display_from_value(&args[2])?;
                with_editor_mut(|st| {
                    if let Some(e) = st.focused_buf_mut().props_mut().overlay_mut(id) {
                        e.display = disp;
                    }
                });
            }
            other => {
                return Err(RuntimeError::TypeMismatch {
                    name: "overlay-put".into(),
                    expected: "face|priority|pad-to-width|display".into(),
                    got: other.into(),
                });
            }
        }
        ok_unit(env)
    });
    b!("overlay-delete", 1, |args, env| {
        let id = crate::props::OverlayId(as_int(&args[0], "overlay-delete")? as u64);
        with_editor_mut(|st| {
            st.focused_buf_mut().props_mut().delete_overlay(id);
        });
        ok_unit(env)
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
        // Return Str (not Ident) — rizz re-evaluates a native fn's return
        // and a raw ident would try to resolve as a variable.
        Ok((Rc::new(Value::Str(s.into())), env.clone()))
    });

    // the directory that the editor was started in
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
/// render-phase callback — a slot that tries to mutate the buffer it's
/// drawing would corrupt the frame and is almost always a bug. The slot
/// name surrounding the error gets attached upstream by the precompute pass.
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

/// Escape `s` so it can be embedded as a rizz string literal. The
/// notify-via-lisp bridge constructs `(notify "<msg>")` source from
/// arbitrary message text — including eval-error messages with embedded
/// quotes, backslashes, or newlines — so we can't rely on `Value::repr`,
/// which assumes a clean ASCII identifier-like payload.
pub(crate) fn quote_for_lisp(s: &str) -> String {
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

/// Fire `(notify "<msg>")` against `env`. Used by builtins that are already
/// inside an `eval_lisp_value` frame (so they can't recursively call
/// `State::eval_lisp`) — `command-submit` and `evaluate` route their result
/// / error messages through here. A broken user `notify` falls back to
/// recording the original message *and* the failure, but never panics.
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

/// Translate ergonomic minibuffer input into a lisp form.
///
/// * `(...)` passes through as-is.
/// * A bare integer becomes `(line N)` — preserves the legacy `:42` jump.
/// * Anything else is wrapped in parens.
///   `head arg1 arg2 ...` and becomes `(head arg1 arg2 ...)`.
/// * Empty input becomes `()` (a no-op).
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

/// Parse a value into an optional [`crate::render::Display`]. Recognized
/// shapes:
///
/// * `()` — clear any display (returns `None`)
/// * `Str` / `Ident` — display the literal text
/// * `{"text": "..."}` — same as a bare string
/// * `{"space": N}` — N blank cells
fn display_from_value(v: &Rc<Value>) -> Result<Option<crate::render::Display>, RuntimeError> {
    use crate::render::Display;
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

/// Parse an anchor value. Accepted shapes:
///
/// * `'status-left` / `'status-right` / `'decorator` / `'top` / `'bottom`
/// * `{"gutter": N}` — left-side gutter N columns wide
fn parse_anchor(v: &Rc<Value>) -> Result<RegionAnchor, RuntimeError> {
    match &**v {
        Value::Ident(s) | Value::Str(s) => Ok(match s.as_ref() {
            "status-left" => RegionAnchor::StatusLeft,
            "status-right" => RegionAnchor::StatusRight,
            "decorator" => RegionAnchor::Decorator,
            "top" => RegionAnchor::Top,
            "bottom" => RegionAnchor::Bottom,
            other => return Err(unknown_variant("anchor", other)),
        }),
        Value::Map(m) => {
            let key = |k: &str| Rc::new(Value::Str(k.into()));
            if let Some(w) = m.get(&key("gutter")) {
                let width = as_int(w, "anchor.gutter")?.max(0);
                let width = u16::try_from(width).unwrap_or(0);
                return Ok(RegionAnchor::Gutter { width });
            }
            Err(RuntimeError::type_mismatch("anchor", "{gutter: N}", v))
        }
        _ => Err(RuntimeError::type_mismatch("anchor", "ident|str|map", v)),
    }
}

/// Interpret a user-supplied handler:
///
/// * `Value::Ident(s)` — try `BuiltinId::parse(s)`; if it matches and is
///   compatible with the anchor, use the builtin. Mismatches error out.
/// * Callable — `Producer::Callable(...)`.
/// * Anything else — `Producer::Static(...)`.
fn parse_handler(
    v: &Rc<Value>,
    name: &str,
    anchor: &RegionAnchor,
) -> Result<Producer, RuntimeError> {
    if let Value::Ident(s) = &**v
        && let Some(b) = BuiltinId::parse(s)
    {
        if !b.matches_anchor(anchor) {
            return Err(RuntimeError::TypeMismatch {
                name: name.into(),
                expected: format!("builtin compatible with {:?}", anchor).into(),
                got: format!("{:?}", b).into(),
            });
        }
        return Ok(Producer::Builtin(b));
    }
    if v.is_callable() {
        Ok(Producer::Callable(v.clone()))
    } else {
        Ok(Producer::Static(v.clone()))
    }
}

fn parse_mode_ident(v: &Rc<Value>) -> Result<EditingMode, RuntimeError> {
    let s = as_ident(v, "mode")?;
    s.parse().map_err(|_| unknown_variant("mode", &s))
}

/// Loose mode-name parser used by the keymap builtins. Unlike
/// [`parse_mode_ident`] (used by `set-mode`), this accepts any ident or
/// string — that's what lets popup modes coexist with the typed
/// [`EditingMode`] names in the keymap registry.
fn parse_mode_name(v: &Rc<Value>) -> Result<Rc<str>, RuntimeError> {
    as_ident_or_str(v, "mode")
}

/// Parse a popup's `modes` property. Accepts a single mode name (ident/str)
/// or an array of mode names. The returned vec is ordered least-recent
/// first — the last entry becomes the topmost layer once pushed.
fn parse_mode_layers(v: &Rc<Value>) -> Result<Vec<Rc<str>>, RuntimeError> {
    match &**v {
        Value::Array(items) => items.iter().map(parse_mode_name).collect(),
        _ => Ok(vec![parse_mode_name(v)?]),
    }
}

// ---------------------------------------------------------------------------
// Popup property parsing
// ---------------------------------------------------------------------------

/// Build a [`PopupSpec`] from the property map passed to `popup-open`.
/// Recognized top-level keys (all optional):
///
/// * `"text"`        — initial text content (str).
/// * `"mode"`        — keymap mode name (str / ident). Default `"popup"`.
/// * `"buffer-mode"` — editing mode for the popup buffer (str / ident). Default `"normal"`.
/// * `"placement"`   — see [`parse_placement`].
/// * `"border"`      — `"none" | "plain" | "rounded" | "double" | "thick"`.
/// * `"title"`       — str shown in the top border.
/// * `"face"`        — face for the popup's background fill.
/// * `"border-face"` — face for the border characters.
/// * `"title-face"`  — face for the title text.
/// * `"show-cursor"` — truthy/falsy. Off by default.
/// * `"wrap-mode"`   — see [`WrapMode::from_str`]
/// * `"wrap-column"` — Fixed wrap column.
/// * `"break-indent"` — Indent continuation rows under the original line's leading whitespace.
///
/// A bare string is also accepted and treated as `{"text": <string>}` so
/// `(popup-open "hi")` works for quick demos.
fn parse_popup_props(v: &Rc<Value>) -> Result<PopupSpec, RuntimeError> {
    let mut spec = PopupSpec::new();
    match &**v {
        Value::Unit => Ok(spec),
        Value::Str(s) | Value::Ident(s) => {
            spec.initial_text = Some(s.to_string());
            Ok(spec)
        }
        Value::Map(m) => {
            let key = |k: &str| Rc::new(Value::Str(k.into()));
            if let Some(t) = m.get(&key("text")) {
                spec.initial_text = Some(as_str(t, "popup-open.text")?.to_string());
            }
            // `modes` (array) is the canonical form; `mode` (single name)
            // is a convenience shorthand for a one-layer stack.
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
            if let Some(b) = m.get(&key("border")) {
                spec.chrome.border = parse_border(b)?;
            }
            if let Some(t) = m.get(&key("title")) {
                spec.chrome.title = Some(as_str(t, "popup-open.title")?);
            }
            if let Some(f) = m.get(&key("face")) {
                spec.chrome.face = Some(as_ident_or_str(f, "popup-open.face")?);
            }
            if let Some(f) = m.get(&key("border-face")) {
                spec.chrome.border_face = Some(as_ident_or_str(f, "popup-open.border-face")?);
            }
            if let Some(f) = m.get(&key("title-face")) {
                spec.chrome.title_face = Some(as_ident_or_str(f, "popup-open.title-face")?);
            }
            if let Some(sc) = m.get(&key("show-cursor")) {
                spec.show_cursor = sc.is_truthy();
            }
            if let Some(sc) = m.get(&key("wrap-mode")) {
                spec.wrap_mode = WrapMode::from_str(&as_ident_or_str(sc, "popup-open.wrap-mode")?)
                    .unwrap_or_default();
            }
            if let Some(sc) = m.get(&key("wrap-column")) {
                spec.wrap_column = Some(as_int(sc, "popup-open.wrap-column")?.min(0) as u16)
            }
            if let Some(sc) = m.get(&key("break-indent")) {
                spec.breakindent = sc.is_truthy();
            }
            Ok(spec)
        }
        _ => Err(RuntimeError::type_mismatch(
            "popup-open",
            "map | str | ()",
            v,
        )),
    }
}

/// Parse a placement value. Accepted shapes:
///
/// * `'center` / `'full` — preset shapes with defaults.
/// * `{"kind": "center" "w": <dim> "h": <dim>}` — centered, sized.
/// * `{"kind": "at" "x": N "y": N "w": <dim> "h": <dim>}` — fixed corner.
/// * `{"kind": "side" "side": "top|bottom|left|right" "size": <dim>}` —
///   pinned to one edge.
/// * `{"kind": "full"}` — full editor area.
///
/// `<dim>` is either an int (cell count) or a float (fraction in `[0, 1]`).
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

/// Parse a dimension: int → cell count, float → fraction of the available
/// axis. The fractional form makes `{"placement": {"kind": "center" "w": 0.5}}`
/// readable in lisp.
fn parse_dim(v: &Rc<Value>) -> Result<Dim, RuntimeError> {
    match &**v {
        Value::Int(n) => Ok(Dim::Cells((*n).max(0) as u16)),
        Value::Float(f) => Ok(Dim::Frac(f.into_inner() as f32)),
        _ => Err(RuntimeError::type_mismatch("dim", "int|float", v)),
    }
}

fn parse_border(v: &Rc<Value>) -> Result<BorderStyle, RuntimeError> {
    let s = as_ident_or_str(v, "border")?;
    Ok(match s.as_ref() {
        "none" => BorderStyle::None,
        "plain" => BorderStyle::Plain,
        "rounded" => BorderStyle::Rounded,
        "double" => BorderStyle::Double,
        "thick" => BorderStyle::Thick,
        other => return Err(unknown_variant("border", other)),
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
        let c = crate::styling::color_from_value(&v).unwrap();
        assert_eq!(c, Some(crate::styling::Color::Rgb(60, 90, 130)));
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
                let style = m
                    .get(&Rc::new(Value::Str("style".into())))
                    .expect("style field");
                // Face references are normalized to Str so they survive
                // rizz's post-call re-eval.
                assert!(matches!(&**style, Value::Str(s) if s.as_ref() == "header"));
            }
            other => panic!("expected map, got {other:?}"),
        }
    }

    #[test]
    fn status_segment_add_via_lisp_changes_frame() {
        let mut s = test_state();
        s.eval_lisp(r#"(region-add 'star 'status-right "★")"#)
            .unwrap();
        let (frame, err) = s.precompute_frame();
        assert!(err.is_none(), "no slot errors: {err:?}");
        let texts: Vec<&str> = frame
            .status_right
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(texts.contains(&"★"), "star segment missing: {texts:?}");
    }

    #[test]
    fn status_segment_remove_via_lisp_drops_segment() {
        let mut s = test_state();
        // Clear every default left segment so we can check removal works.
        s.eval_lisp("(region-remove 'mode)").unwrap();
        s.eval_lisp("(region-remove 'buffer-path)").unwrap();
        s.eval_lisp("(region-remove 'sel-hint)").unwrap();
        let (frame, err) = s.precompute_frame();
        assert!(err.is_none());
        assert!(frame.status_left.is_empty());
    }

    #[test]
    fn callable_status_segment_runs_each_frame() {
        let mut s = test_state();
        // Strip out the theme's left segments so our test segment is the
        // sole content on the left.
        s.eval_lisp("(region-remove 'mode)").unwrap();
        s.eval_lisp("(region-remove 'buffer-path)").unwrap();
        s.eval_lisp("(region-remove 'sel-hint)").unwrap();
        s.eval_lisp(r#"(region-add 'probe 'status-left (fn _p () (focused-mode)))"#)
            .unwrap();
        let (frame, _err) = s.precompute_frame();
        let s_left: Vec<&str> = frame
            .status_left
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(s_left, vec!["normal"]);
    }

    #[test]
    fn status_segment_with_cjk_uses_display_width() {
        // A wide-char segment must contribute its display width, not its
        // char count. Strip the theme's right segments so the math is easy
        // to follow.
        use unicode_width::UnicodeWidthStr;
        let mut s = test_state();
        for name in ["cursor", "pip", "last-key", "spacer", "bufno"] {
            s.eval_lisp(&format!("(region-remove '{name})")).unwrap();
        }
        s.eval_lisp(r#"(region-add 'cjk 'status-right "漢字")"#)
            .unwrap();
        let (frame, _) = s.precompute_frame();
        let total: usize = frame
            .status_right
            .iter()
            .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
            .sum();
        // "漢字" — two wide characters → 4 columns.
        assert_eq!(total, 4);
    }

    #[test]
    fn render_phase_blocks_mutating_builtins() {
        // A render callback that calls `(insert-char "x")` must error out and
        // leave the buffer alone.
        let mut s = test_state();
        s.eval_lisp(
            r#"(region-add 'naughty 'status-left
                  (fn _naughty () (do (insert-char "x") "")))"#,
        )
        .unwrap();
        let pre = s.focused_buf().text();
        let (_, err) = s.precompute_frame();
        assert!(err.is_some(), "expected a render-phase error");
        let after = s.focused_buf().text();
        assert_eq!(pre, after, "render callback must not mutate the buffer");
    }

    #[test]
    fn put_text_property_shows_up_in_frame_decorators() {
        let mut s = test_state();
        // Need some content so the prop range maps to a visible row, and a
        // viewport so `build_prop_ranges` doesn't short-circuit.
        s.eval_lisp("(set-mode 'insert)").unwrap();
        s.eval_lisp("(insert \"hello world\")").unwrap();
        s.eval_lisp("(set-mode 'normal)").unwrap();
        s.eval_lisp(r#"(put-text-property 0 0 0 5 "twilight.accent")"#)
            .unwrap();
        let (frame, err) = s.precompute_frame();
        assert!(err.is_none(), "{err:?}");
        let found = frame
            .per_buf
            .iter()
            .flat_map(|b| b.decorators.iter())
            .flat_map(|d| d.ranges.iter())
            .any(|r| r.row == 0 && r.col == 0 && r.len == 5);
        assert!(found, "expected a styled range from the text property");
    }

    #[test]
    fn overlay_create_put_delete_round_trip() {
        let mut s = test_state();
        s.eval_lisp("(set-mode 'insert)").unwrap();
        s.eval_lisp("(insert \"abcde\")").unwrap();
        s.eval_lisp("(set-mode 'normal)").unwrap();
        let id = s
            .eval_lisp(r#"(overlay-create 0 0 0 3 "twilight.accent")"#)
            .unwrap();
        let id_int = id.as_int().expect("overlay-create returns int");
        // Mutate it.
        s.eval_lisp(&format!(r#"(overlay-put {id_int} "priority" 5)"#))
            .unwrap();
        let (frame, _) = s.precompute_frame();
        let has_range = frame
            .per_buf
            .iter()
            .flat_map(|b| b.decorators.iter())
            .flat_map(|d| d.ranges.iter())
            .any(|r| r.row == 0 && r.col == 0 && r.len == 3);
        assert!(has_range, "overlay should have produced a range");
        // Delete and confirm.
        s.eval_lisp(&format!("(overlay-delete {id_int})")).unwrap();
        let (frame2, _) = s.precompute_frame();
        let still = frame2
            .per_buf
            .iter()
            .flat_map(|b| b.decorators.iter())
            .flat_map(|d| d.ranges.iter())
            .any(|r| r.row == 0 && r.col == 0 && r.len == 3);
        assert!(!still, "deleted overlay shouldn't appear");
    }

    #[test]
    fn overlay_display_substitution_lands_in_styled_range() {
        let mut s = test_state();
        s.eval_lisp("(set-mode 'insert)").unwrap();
        s.eval_lisp("(insert \"abcdefghij\")").unwrap();
        s.eval_lisp("(set-mode 'normal)").unwrap();
        let id = s
            .eval_lisp(r#"(overlay-create 0 2 0 8 "twilight.muted")"#)
            .unwrap();
        let id_int = id.as_int().unwrap();
        s.eval_lisp(&format!(r#"(overlay-put {id_int} "display" "...")"#))
            .unwrap();
        let (frame, _) = s.precompute_frame();
        let got_display = frame
            .per_buf
            .iter()
            .flat_map(|b| b.decorators.iter())
            .flat_map(|d| d.ranges.iter())
            .find(|r| r.row == 0 && r.col == 2 && r.len == 6)
            .and_then(|r| r.display.clone());
        match got_display {
            Some(crate::render::Display::String(s)) => assert_eq!(&*s, "..."),
            other => panic!("expected Display::String(...), got {other:?}"),
        }
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
