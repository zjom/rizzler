//! Helpers shared across every builtin module: the [`Builtins`] registration
//! sink, value-conversion shims, mode-name parsers, and a couple of utilities
//! that bridge into the live editor (`apply`, `notify_via_env`).

use std::rc::Rc;

use anyhow::anyhow;
use rizz::runtime::{Env, NativeFn, RuntimeError, Value};
use tracing::{trace, warn};

use rizz_actions::Action;
use rizz_core::EditingMode;

use super::{in_render_phase, with_editor_mut};

/// Accumulates `(name, NativeFn)` entries plus deferred aliases, then folds
/// them into a single [`Env`] via [`Builtins::build`]. Each builtin module
/// receives a `&mut Builtins` and registers its functions through one of the
/// `be*` / `bi*` methods (mirroring the original `be!` / `bi!` macros).
pub(super) struct Builtins {
    entries: Vec<(&'static str, NativeFn)>,
    aliases: Vec<(&'static str, &'static str)>,
}

impl Builtins {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            aliases: Vec::new(),
        }
    }

    /// Register a `WithEnv` builtin (reads env, returns a value only).
    pub fn be<F>(&mut self, name: &'static str, nargs: usize, f: F)
    where
        F: Fn(&[Rc<Value>], &Env) -> Result<Rc<Value>, RuntimeError> + 'static,
    {
        self.entries
            .push((name, NativeFn::with_env(name.into(), nargs, f)));
    }

    /// Register a `WithEnv` builtin with an attached doc string.
    pub fn be_doc<F>(&mut self, name: &'static str, nargs: usize, f: F, doc: &'static str)
    where
        F: Fn(&[Rc<Value>], &Env) -> Result<Rc<Value>, RuntimeError> + 'static,
    {
        self.entries.push((
            name,
            NativeFn::with_env(name.into(), nargs, f).with_doc(Rc::from(doc)),
        ));
    }

    /// Register an `Impure` builtin (may return an extended env that the
    /// evaluator threads back into the caller's scope).
    pub fn bi<F>(&mut self, name: &'static str, nargs: usize, f: F)
    where
        F: Fn(&[Rc<Value>], &Env) -> Result<(Rc<Value>, Env), RuntimeError> + 'static,
    {
        self.entries
            .push((name, NativeFn::impure(name.into(), nargs, f)));
    }

    /// Register an `Impure` builtin with an attached doc string.
    pub fn bi_doc<F>(&mut self, name: &'static str, nargs: usize, f: F, doc: &'static str)
    where
        F: Fn(&[Rc<Value>], &Env) -> Result<(Rc<Value>, Env), RuntimeError> + 'static,
    {
        self.entries.push((
            name,
            NativeFn::impure(name.into(), nargs, f).with_doc(Rc::from(doc)),
        ));
    }

    /// Bind `a` as a second name for the value already registered under `t`.
    /// Aliases are resolved after every primary entry has been inserted, so
    /// order vs. `be*`/`bi*` calls does not matter.
    pub fn alias(&mut self, a: &'static str, t: &'static str) {
        self.aliases.push((a, t));
    }

    pub fn build(self) -> Env {
        let mut env = Env::of_builtins(self.entries);
        for (a, t) in self.aliases {
            let v = env.get(&Rc::<str>::from(t)).expect("alias target").clone();
            env = env.update(a.into(), v);
        }
        env
    }
}

pub(super) fn unit() -> Rc<Value> {
    Rc::new(Value::Unit)
}

/// Run `action` against the live `State`. Errors when called from inside a
/// render-phase callback.
pub(super) fn apply(action: Action) -> Result<(), RuntimeError> {
    if in_render_phase() {
        warn!(
            ?action,
            "lisp builtin attempted to mutate during render phase"
        );
        return Err(RuntimeError::TypeMismatch {
            name: "editor-action".into(),
            expected: "non-mutating call".into(),
            got: "called from a render callback".into(),
        });
    }
    trace!(?action, "lisp -> action");
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
pub(super) fn notify_via_env(msg: &str, env: &Env) {
    trace!(msg, "notify_via_env");
    let src = format!("(notify {})", quote_for_lisp(msg));
    if let Err(e) = rizz::parse_and_run_with_env(src.as_bytes(), env) {
        warn!(error = %e, msg, "notify_via_env failed -> falling back to journal");
        with_editor_mut(|st| {
            st.record_message(msg);
            st.record_message(&format!("notify failed: {e}"));
        });
    }
}

pub(super) fn as_str(v: &Rc<Value>, name: &str) -> Result<Rc<str>, RuntimeError> {
    v.as_str()
        .ok_or_else(|| RuntimeError::type_mismatch(name, "str", v))
}

pub(super) fn as_int(v: &Rc<Value>, name: &str) -> Result<i64, RuntimeError> {
    v.as_int()
        .ok_or_else(|| RuntimeError::type_mismatch(name, "int", v))
}

pub(super) fn as_ident(v: &Rc<Value>, name: &str) -> Result<Rc<str>, RuntimeError> {
    match &**v {
        Value::Ident(s) => Ok(s.clone()),
        _ => Err(RuntimeError::type_mismatch(name, "ident", v)),
    }
}

pub(super) fn as_ident_or_str(v: &Rc<Value>, name: &str) -> Result<Rc<str>, RuntimeError> {
    match &**v {
        Value::Ident(s) | Value::Str(s) => Ok(s.clone()),
        _ => Err(RuntimeError::type_mismatch(name, "ident|str", v)),
    }
}

pub(super) fn wrap_shell_style(input: &str) -> String {
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

pub(super) fn as_u8(v: &Rc<Value>, name: &str) -> Result<u8, RuntimeError> {
    let n = as_int(v, name)?;
    u8::try_from(n).map_err(|_| RuntimeError::TypeMismatch {
        name: name.into(),
        expected: "0..=255".into(),
        got: n.to_string().into(),
    })
}

pub(super) fn as_usize(v: &Rc<Value>, name: &str) -> Result<usize, RuntimeError> {
    let n = as_int(v, name)?;
    usize::try_from(n).map_err(|_| RuntimeError::TypeMismatch {
        name: name.into(),
        expected: "0..=usize::MAX".into(),
        got: n.to_string().into(),
    })
}

pub(super) fn display_from_value(
    v: &Rc<Value>,
) -> Result<Option<rizz_core::Display>, RuntimeError> {
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

pub(super) fn parse_mode_ident(v: &Rc<Value>) -> Result<EditingMode, RuntimeError> {
    let s = as_ident(v, "mode")?;
    s.parse().map_err(|_| unknown_variant("mode", &s))
}

pub(super) fn parse_mode_name(v: &Rc<Value>) -> Result<Rc<str>, RuntimeError> {
    as_ident_or_str(v, "mode")
}

pub(super) fn parse_mode_layers(v: &Rc<Value>) -> Result<Vec<Rc<str>>, RuntimeError> {
    match &**v {
        Value::Array(items) => items.iter().map(parse_mode_name).collect(),
        _ => Ok(vec![parse_mode_name(v)?]),
    }
}

pub(super) fn unknown_variant(name: &str, got: &str) -> RuntimeError {
    RuntimeError::TypeMismatch {
        name: name.into(),
        expected: "known symbol".into(),
        got: got.into(),
    }
}

pub(super) fn str_mismatch(name: &str, expected: &str) -> RuntimeError {
    RuntimeError::TypeMismatch {
        name: name.into(),
        expected: expected.into(),
        got: "?".into(),
    }
}

pub(super) fn str_mismatch_msg(name: &str, msg: &str) -> RuntimeError {
    RuntimeError::TypeMismatch {
        name: name.into(),
        expected: "valid key sequence".into(),
        got: msg.into(),
    }
}
