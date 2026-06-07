use std::rc::Rc;

use im::Vector;
use rizz::runtime::Value;
use rizz_actions::Action;

use super::super::helpers::{
    Builtins, apply, as_str, notify_via_env, unit, wrap_shell_style,
};
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
}
