//! LSP client builtins: register/install servers, request hover/completion/
//! code-action, and structured callback hooks for the lisp UI.

use std::rc::Rc;

use anyhow::anyhow;
use rizz::runtime::{RuntimeError, Value};

use rizz_actions::Action;
use rizz_lsp_install::InstallOpts;

use super::super::helpers::{Builtins, as_ident_or_str, as_str, as_usize, unit};
use super::super::with_editor_mut;

pub(super) fn register(b: &mut Builtins) {
    // Escape hatch: bypass lsp.toml entirely. Useful for ad-hoc servers.
    b.be_doc(
        "lsp-register",
        4,
        |args, _| {
            let name = as_str(&args[0], "lsp-register")?;
            let command = as_str(&args[1], "lsp-register")?;
            let args_list = parse_string_list(&args[2], "lsp-register.args")?;
            let exts = parse_string_list(&args[3], "lsp-register.exts")?;
            with_editor_mut(|st| {
                st.register_lsp_server(
                    &name,
                    command.to_string(),
                    args_list,
                    exts,
                    Vec::new(),
                );
            });
            Ok(unit())
        },
        "(lsp-register NAME COMMAND ARGS EXTS)\n\nRegisters an LSP server programmatically, bypassing lsp.toml. An escape\nhatch for ad-hoc servers — (lsp-install NAME) is the curated path.\n\nNAME    — str: the server name.\nCOMMAND — str: the executable to launch.\nARGS    — array of str: launch arguments (or () for none).\nEXTS    — array of str: file extensions this server handles.\nSee also: (lsp-install NAME), (lsp-restart [NAME]).",
    );

    b.be_doc(
        "lsp-install",
        1,
        |args, _| {
            let name = as_ident_or_str(&args[0], "lsp-install")?;
            let opts = if args.len() >= 2 {
                parse_install_opts(&args[1])?
            } else {
                InstallOpts::default()
            };
            let install_res = with_editor_mut(|st| st.install_lsp_server(&name, opts));
            Ok(Rc::new(match install_res {
                Ok(path) => Value::Cons {
                    head: Rc::new(Value::Ident("ok".into())),
                    tail: Rc::new(Value::Str(path.display().to_string().into())),
                },
                Err(msg) => Value::Cons {
                    head: Rc::new(Value::Ident("err".into())),
                    tail: Rc::new(Value::Str(msg.into())),
                },
            }))
        },
        r#"(lsp-install NAME [OPTS])

Installs an LSP server by NAME from the curated lsp.toml manifest. Runs
the install recipe if the binary is missing from $PATH and the cache.
Idempotent on cache hit.

NAME — ident | str: the server name.
OPTS — map: optional overrides. Recognized keys:
         "command": str — override the binary name
         "args":    array of str — override the launch args
         "install": str — override the install recipe
         "force":   truthy — re-run the recipe even on cache hit

Returns a pair: (ok . PATH) on success, or (err . MESSAGE) on failure.
See also: (lsp-installed? NAME), (set-lsp-auto-install ON),
(lsp-register ...)."#,
    );

    b.be_doc(
        "lsp-installed?",
        1,
        |args, _| {
            let name = as_ident_or_str(&args[0], "lsp-installed?")?;
            let installed = with_editor_mut(|st| st.lsp_installed(&name));
            Ok(Rc::new(installed.into()))
        },
        "(lsp-installed? NAME)\n\nReturns 1 if the cache or $PATH holds a binary for the LSP server NAME,\nelse 0.\n\nNAME — ident | str: the server name.\nSee also: (lsp-install NAME).",
    );

    b.be_doc(
        "set-lsp-auto-install",
        1,
        |args, _| {
            let on = args[0].is_truthy();
            with_editor_mut(|st| st.set_lsp_auto_install(on));
            Ok(unit())
        },
        "(set-lsp-auto-install ON)\n\nToggles automatic install of LSP servers on file open. When ON (the\ndefault), opening a file whose extension matches an lsp.toml entry runs\nthe install recipe if the binary is missing.\n\nON — int: nonzero to enable, 0 to disable.\nSee also: (lsp-auto-install?), (lsp-install NAME).",
    );

    b.be_doc(
        "lsp-auto-install?",
        0,
        |_, _| {
            let on = with_editor_mut(|st| st.lsp_auto_install());
            Ok(Rc::new(on.into()))
        },
        "(lsp-auto-install?)\n\nReturns 1 if LSP servers are auto-installed on first file open (the\ndefault), else 0.\nSee also: (set-lsp-auto-install ON).",
    );

    b.be_doc(
        "lsp-hover",
        0,
        |_, _| {
            with_editor_mut(|st| {
                st.apply(&[Rc::new(Action::LspHover)]);
            });
            Ok(unit())
        },
        "(lsp-hover)\n\nRequests textDocument/hover at the cursor and shows the result via\n(notify). A no-op when no server is attached to the focused buffer.\nSee also: (lsp-goto-definition), (lsp-completion).",
    );

    b.be_doc(
        "lsp-goto-definition",
        0,
        |_, _| {
            with_editor_mut(|st| {
                st.apply(&[Rc::new(Action::LspGotoDefinition)]);
            });
            Ok(unit())
        },
        "(lsp-goto-definition)\n\nRequests textDocument/definition at the cursor and jumps to the first\nlocation returned. A no-op when no server is attached.\nSee also: (lsp-hover), (lsp-completion).",
    );

    b.be_doc(
        "lsp-completion",
        0,
        |_, _| {
            with_editor_mut(|st| {
                st.apply(&[Rc::new(Action::LspCompletion)]);
            });
            Ok(unit())
        },
        "(lsp-completion)\n\nRequests textDocument/completion at the cursor. The response is handed\nto the (set-lsp-completion-fn) callback if one is installed, else the\nfirst item is surfaced via (notify).\nSee also: (set-lsp-completion-fn FN), (lsp-apply-completion ID).",
    );

    b.be_doc(
        "lsp-format",
        0,
        |_, _| {
            with_editor_mut(|st| {
                st.apply(&[Rc::new(Action::LspFormat)]);
            });
            Ok(unit())
        },
        "(lsp-format)\n\nRequests textDocument/formatting for the focused buffer and applies the\nreturned edits as a single tracked (undoable) change. A no-op when no\nserver is attached.\nSee also: (lsp-code-action).",
    );

    b.be_doc(
        "lsp-code-action",
        0,
        |_, _| {
            with_editor_mut(|st| {
                st.apply(&[Rc::new(Action::LspCodeAction)]);
            });
            Ok(unit())
        },
        "(lsp-code-action)\n\nRequests textDocument/codeAction at the cursor. The response is handed\nto the (set-lsp-code-action-fn) callback if one is installed, else the\nfirst action is surfaced via (notify).\nSee also: (set-lsp-code-action-fn FN), (lsp-invoke-code-action ID).",
    );

    b.be_doc(
        "lsp-restart",
        0,
        |args, _| {
            let name = if args.is_empty() {
                None
            } else {
                Some(as_ident_or_str(&args[0], "lsp-restart")?.to_string())
            };
            with_editor_mut(|st| st.lsp_restart(name.as_deref()));
            Ok(unit())
        },
        "(lsp-restart [NAME])\n\nShuts down a running LSP client and re-attaches the focused buffer.\n\nNAME — ident | str: optional server to restart. With no argument,\n       restarts the server for the focused buffer's language.\nSee also: (lsp-install NAME), (lsp-register ...).",
    );

    b.be_doc(
        "set-lsp-completion-fn",
        1,
        |args, _| {
            let v = args[0].clone();
            let opt = if v.is_unit() { None } else { Some(v) };
            with_editor_mut(|st| st.set_lsp_completion_fn(opt));
            Ok(unit())
        },
        r#"(set-lsp-completion-fn FN)

Installs the lisp callback for textDocument/completion responses.

FN — fn: receives (ITEMS ANCHOR). Pass () to clear the callback — the
     editor then falls back to a one-line notify summary of the first
     item.
       ITEMS  — array of map: each {"id": int, "label": str,
                "detail": str | (), "insert-text": str,
                "kind": 'function | 'method | 'variable | …}. An empty
                array means no completions (or the batch was cleared) —
                use it to dismiss a popup the callback opened earlier.
       ANCHOR — map {"row": int, "col": int}: where the request was
                issued.

Example:
  (fn _completion-popup (items anchor)
    (popup-show 'completion
      (w-block {"border": "rounded"} (w-popup-self))
      {"text": (str-join (fmap (fn (i) (get i "label")) items) "\n")
       "placement": (placement-anchored 'bottom 'fit)}))
  (set-lsp-completion-fn _completion-popup)
See also: (lsp-completion), (lsp-apply-completion ID)."#,
    );

    b.be_doc(
        "set-lsp-code-action-fn",
        1,
        |args, _| {
            let v = args[0].clone();
            let opt = if v.is_unit() { None } else { Some(v) };
            with_editor_mut(|st| st.set_lsp_code_action_fn(opt));
            Ok(unit())
        },
        r#"(set-lsp-code-action-fn FN)

Installs the lisp callback for textDocument/codeAction responses.

FN — fn: receives (ACTIONS). Pass () to clear the callback — the editor
     then falls back to a one-line notify summary of the first action.
       ACTIONS — array of map: each {"id": int, "title": str,
                 "kind": str | (), "has-edit": 0|1, "has-command": 0|1}.
                 An empty array means no actions at the cursor.

Call (lsp-invoke-code-action ID) from inside the picker to apply the
chosen action's edit and command.
See also: (lsp-code-action), (lsp-invoke-code-action ID)."#,
    );

    b.be_doc(
        "lsp-apply-completion",
        1,
        |args, _| {
            let id = as_usize(&args[0], "lsp-apply-completion.id")?;
            with_editor_mut(|st| st.apply_lsp_completion_by_id(id));
            Ok(unit())
        },
        r#"(lsp-apply-completion ID)

Applies the completion item ID from the most recent
textDocument/completion batch — inserts its "insert-text" at the
originating buffer's cursor.

ID — int: the "id" field from an item map handed to the
     (set-lsp-completion-fn) callback.

A no-op (with a notify) when ID is out of range or no batch is pending.
If you opened a picker popup, call (popup-hide 'your-popup) separately.
See also: (set-lsp-completion-fn FN), (lsp-completion)."#,
    );

    b.be_doc(
        "lsp-invoke-code-action",
        1,
        |args, _| {
            let id = as_usize(&args[0], "lsp-invoke-code-action.id")?;
            with_editor_mut(|st| st.invoke_lsp_code_action_by_id(id));
            Ok(unit())
        },
        r#"(lsp-invoke-code-action ID)

Invokes the code action ID from the most recent textDocument/codeAction
batch — applies its workspace edit if any, then forwards its server
command (if any) via workspace/executeCommand.

ID — int: the "id" field from an action map handed to the
     (set-lsp-code-action-fn) callback.

A no-op (with a notify) when ID is out of range or no batch is pending.
See also: (set-lsp-code-action-fn FN), (lsp-code-action)."#,
    );
}

fn parse_string_list(v: &Rc<Value>, ctx: &'static str) -> Result<Vec<String>, RuntimeError> {
    match v.as_ref() {
        Value::Array(items) => items
            .iter()
            .map(|x| as_str(x, ctx).map(|s| s.to_string()))
            .collect(),
        Value::Unit => Ok(Vec::new()),
        _ => Ok(vec![as_str(v, ctx)?.to_string()]),
    }
}

fn parse_install_opts(v: &Rc<Value>) -> Result<InstallOpts, RuntimeError> {
    let map = match v.as_ref() {
        Value::Map(m) => m,
        _ => {
            return Err(RuntimeError::Other(anyhow!(
                "lsp-install.opts: expected a map"
            )));
        }
    };
    let mut opts = InstallOpts::default();
    if let Some(v) = map.get(&Value::Str("command".into())) {
        opts.command = Some(as_str(v, "lsp-install.command")?.to_string());
    }
    if let Some(v) = map.get(&Value::Str("args".into())) {
        opts.args = Some(parse_string_list(v, "lsp-install.args")?);
    }
    if let Some(v) = map.get(&Value::Str("install".into())) {
        opts.install = Some(as_str(v, "lsp-install.install")?.to_string());
    }
    if let Some(v) = map.get(&Value::Str("force".into())) {
        opts.force = v.is_truthy();
    }
    Ok(opts)
}
