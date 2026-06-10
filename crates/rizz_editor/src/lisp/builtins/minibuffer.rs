//! Minibuffer / command-line builtins: submit, cancel, evaluate, history,
//! and tab-completion against the lisp env.

use std::rc::Rc;

use im::Vector;
use rizz::runtime::Value;
use rizz_actions::Action;

use super::super::helpers::{Builtins, apply, as_str, notify_via_env, unit, wrap_shell_style};
use super::super::with_editor_mut;

pub(super) fn register(b: &mut Builtins) {
    b.bi("command-submit", 0, |_, env| {
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

    b.be("command-cancel", 0, |_, _| {
        apply(Action::CommandCancel)?;
        Ok(unit())
    });

    b.bi("evaluate", 0, |_, env| {
        let src = {
            with_editor_mut(|st| {
                st.focused_buf()
                    .selected_text()
                    .unwrap_or_else(|| st.focused_buf().text())
            })
        };
        // The rizz parser asserts on empty input; an empty buffer or
        // selection is a no-op eval, not a crash.
        if src.trim().is_empty() {
            return Ok((unit(), env.clone()));
        }
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

    b.be("notify-record", 1, |args, _| {
        let s = as_str(&args[0], "notify-record")?;
        with_editor_mut(|st| st.record_message(&s));
        Ok(unit())
    });
    b.be("message-history", 0, |_, _| {
        let msgs: Vector<Rc<Value>> = with_editor_mut(|st| {
            st.message_history()
                .map(|s| Rc::new(Value::Str(s.clone())))
                .collect()
        });
        Ok(Rc::new(Value::Array(msgs)))
    });
    b.be("command-history", 0, |_, _| {
        let cmds: Vector<Rc<Value>> = with_editor_mut(|st| {
            st.cmd_history()
                .map(|s| Rc::new(Value::Str(s.clone())))
                .collect()
        });
        Ok(Rc::new(Value::Array(cmds)))
    });

    b.be("command-prefix", 0, |_, _| {
        let s = with_editor_mut(|st| st.minibuffer_completion_prefix());
        Ok(Rc::new(Value::Str(s.into())))
    });

    // `Env::filter` with an always-true predicate is the supported way to
    // iterate the (private) bindings map. The clone is O(1) thanks to
    // `im::HashMap`'s persistent representation.
    b.be("command-completions", 0, |_, env| {
        let prefix = with_editor_mut(|st| st.minibuffer_completion_prefix());
        let mut names: Vec<Rc<str>> = Vec::new();
        let _ = env.clone().filter(|(name, _)| {
            if name.starts_with(prefix.as_str()) && !name.starts_with('_') {
                names.push(name.clone());
            }
            true
        });
        names.sort();
        let arr: Vector<Rc<Value>> = names.into_iter().map(|s| Rc::new(Value::Str(s))).collect();
        Ok(Rc::new(Value::Array(arr)))
    });

    b.be("command-complete", 1, |args, _| {
        let s = as_str(&args[0], "command-complete")?;
        with_editor_mut(|st| st.apply_minibuffer_completion(&s));
        Ok(unit())
    });

    b.be("longest-common-prefix", 1, |args, _| {
        let arr = args[0].as_array().ok_or_else(|| {
            rizz::runtime::RuntimeError::type_mismatch("longest-common-prefix", "array", &args[0])
        })?;
        let mut strings: Vec<Rc<str>> = Vec::with_capacity(arr.len());
        for v in arr.iter() {
            strings.push(as_str(v, "longest-common-prefix")?);
        }
        let s = crate::completion::longest_common_prefix(strings.iter().map(|s| s.as_ref()));
        Ok(Rc::new(Value::Str(s.into())))
    });
}
