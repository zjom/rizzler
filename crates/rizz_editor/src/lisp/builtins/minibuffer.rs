//! Minibuffer / command-line builtins: submit, cancel, evaluate, history,
//! and tab-completion against the lisp env.

use std::rc::Rc;

use im::Vector;
use rizz::runtime::{Env, Value};
use rizz_actions::Action;

use super::super::helpers::{Builtins, apply, as_str, notify_via_env, unit, wrap_shell_style};
use super::super::{current_toplevel_env, with_editor_mut};

pub(super) fn register(b: &mut Builtins) {
    b.bi_doc(
        "command-submit",
        0,
        |_, env| {
            let cmd = with_editor_mut(|st| st.take_minibuffer_command());
            with_editor_mut(|st| st.record_cmd(&cmd));
            let src = wrap_shell_style(&cmd);
            // Run against the live top-level env, not the lexical env captured
            // by the closure that called us (`_menu-command-submit`): that
            // snapshot predates user fns defined later in init.rz, so a typed
            // `popup-help` would otherwise resolve to nothing.
            let live = current_toplevel_env();
            let run_env: &Env = live.as_ref().unwrap_or(env);
            match rizz::parse_and_run_with_env(src.as_bytes(), run_env) {
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
        },
        "(command-submit)\n\nRuns the text in the command minibuffer as a lisp form and records it in\ncommand history. Bare shell-style input like `edit foo.txt` is wrapped\ninto a call; a non-() result is shown via (notify). Bound to Enter in\nthe command prompt.\nSee also: (command-cancel), (evaluate), (command-history).",
    );

    b.be_doc(
        "command-cancel",
        0,
        |_, _| {
            apply(Action::CommandCancel)?;
            Ok(unit())
        },
        "(command-cancel)\n\nDismisses the command minibuffer without running anything.\nSee also: (command-submit).",
    );

    b.be_doc(
        "command-history-prev",
        0,
        |_, _| {
            with_editor_mut(|st| st.command_history_prev());
            Ok(unit())
        },
        "(command-history-prev)\n\nReplaces the command minibuffer with the previous (older) command from\nhistory, stashing the in-progress line on the first step so\n(command-history-next) can walk back to it. A no-op at the oldest entry\nor with empty history. Bound to <up> in the command prompt.\nSee also: (command-history-next), (command-history).",
    );

    b.be_doc(
        "command-history-next",
        0,
        |_, _| {
            with_editor_mut(|st| st.command_history_next());
            Ok(unit())
        },
        "(command-history-next)\n\nReplaces the command minibuffer with the next (newer) command from\nhistory; stepping past the newest entry restores the line you were\ntyping before recall began. A no-op unless (command-history-prev) has\nbeen used. Bound to <down> in the command prompt.\nSee also: (command-history-prev), (command-history).",
    );

    b.bi_doc(
        "evaluate",
        0,
        |_, env| {
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
        },
        "(evaluate)\n\nEvaluates the focused buffer's active selection as lisp, or the whole\nbuffer if nothing is selected. A non-() result is shown via (notify);\nany bindings introduced persist in the running env. Empty input is a\nno-op. This is how you eval your config buffer live.\nSee also: (command-submit), (reload-config).",
    );

    b.be_doc(
        "notify-record",
        1,
        |args, _| {
            let s = as_str(&args[0], "notify-record")?;
            with_editor_mut(|st| st.record_message(&s));
            Ok(unit())
        },
        "(notify-record MSG)\n\nAppends MSG to the message journal without flashing it (unlike the\n(notify) helper, which also records). Use it to log without disturbing\nthe user.\n\nMSG — str: the message to record.\nSee also: (message-history).",
    );
    b.be_doc(
        "message-history",
        0,
        |_, _| {
            let msgs: Vector<Rc<Value>> = with_editor_mut(|st| {
                st.message_history()
                    .map(|s| Rc::new(Value::Str(s.clone())))
                    .collect()
            });
            Ok(Rc::new(Value::Array(msgs)))
        },
        "(message-history)\n\nReturns array of str: every journalled message, oldest first. Drives an\noptional `:messages` popup.\nSee also: (notify-record MSG), (command-history).",
    );
    b.be_doc(
        "command-history",
        0,
        |_, _| {
            let cmds: Vector<Rc<Value>> = with_editor_mut(|st| {
                st.cmd_history()
                    .map(|s| Rc::new(Value::Str(s.clone())))
                    .collect()
            });
            Ok(Rc::new(Value::Array(cmds)))
        },
        "(command-history)\n\nReturns array of str: every command submitted through the minibuffer,\noldest first.\nSee also: (command-submit), (message-history).",
    );

    b.be_doc(
        "command-prefix",
        0,
        |_, _| {
            let s = with_editor_mut(|st| st.minibuffer_completion_prefix());
            Ok(Rc::new(Value::Str(s.into())))
        },
        "(command-prefix)\n\nReturns str: the word the minibuffer cursor is completing — the token a\ntab-completion callback should match against.\nSee also: (command-completions), (command-complete TEXT).",
    );

    b.be_doc(
        "command-completions",
        0,
        |_, env| {
            let prefix = with_editor_mut(|st| st.minibuffer_completion_prefix());
            // Complete against the live top-level env so user fns defined
            // anywhere in init.rz (or interactively) are visible — not just the
            // lexical snapshot the calling closure (`_command-tab` / `_menu-cands`)
            // captured at definition time.
            let live = current_toplevel_env();
            let src_env: &Env = live.as_ref().unwrap_or(env);
            let mut names: Vec<Rc<str>> = src_env.into_iter().filter(|( name, _ )| {
                name.starts_with(prefix.as_str()) && !name.starts_with('_')
            }).map(|(name,_)| Rc::clone(name)).collect();
            names.sort();
            let arr = names.iter().map(|n| Rc::new(Value::Str(n.clone()))).collect();
            Ok(Rc::new(Value::Array(arr)))
        },
        "(command-completions)\n\nReturns array of str: every binding in the env whose name starts with\nthe current (command-prefix), sorted, skipping underscore-prefixed\nprivate names. The default tab-completion candidate set.\nSee also: (command-prefix), (longest-common-prefix STRS).",
    );

    b.be_doc(
        "command-complete",
        1,
        |args, _| {
            let s = as_str(&args[0], "command-complete")?;
            with_editor_mut(|st| st.apply_minibuffer_completion(&s));
            Ok(unit())
        },
        "(command-complete TEXT)\n\nReplaces the token being completed in the minibuffer with TEXT, the\naction a completion picker invokes once the user chooses an item.\n\nTEXT — str: the replacement token.\nSee also: (command-completions), (command-prefix).",
    );

    b.be_doc(
        "longest-common-prefix",
        1,
        |args, _| {
            let arr = args[0].as_array().ok_or_else(|| {
                rizz::runtime::RuntimeError::type_mismatch(
                    "longest-common-prefix",
                    "array",
                    &args[0],
                )
            })?;
            let mut strings: Vec<Rc<str>> = Vec::with_capacity(arr.len());
            for v in arr.iter() {
                strings.push(as_str(v, "longest-common-prefix")?);
            }
            let s = crate::completion::longest_common_prefix(strings.iter().map(|s| s.as_ref()));
            Ok(Rc::new(Value::Str(s.into())))
        },
        "(longest-common-prefix STRS)\n\nReturns str: the longest string that is a prefix of every entry in\nSTRS, or \"\" if they share none. Used to extend a completion to the\npoint the candidates diverge.\n\nSTRS — array of str.\nSee also: (command-completions).",
    );
}
