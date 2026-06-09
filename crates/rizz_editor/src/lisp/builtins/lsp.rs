use std::rc::Rc;

use anyhow::anyhow;
use rizz::runtime::{RuntimeError, Value};

use rizz_actions::Action;
use rizz_lsp_install::InstallOpts;

use super::super::helpers::{as_ident_or_str, as_str, unit, Builtins};
use super::super::with_editor_mut;

pub(super) fn register(b: &mut Builtins) {
    // Low-level register: bypass lsp.toml entirely. Useful for ad-hoc
    // servers or testing.
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
        "(lsp-register/4)\nregister an LSP server programmatically (bypasses lsp.toml).\nargs: <name str> <command str> <args [str ...]> <extensions [str ...]>",
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
        r#"(lsp-install name opts?)
install an LSP server by name from the curated lsp.toml manifest.
runs the install recipe if the binary is missing from $PATH and the cache.
idempotent on cache hit.

params:
- `name`: str | ident
- `opts`: map
   recognised keys:
   - "command": override the binary name
   - "args":    override the launch args
   - "install": override the install recipe
   - "force":   truthy → re-run the recipe even on cache hit"#,
    );

    b.be_doc(
        "lsp-installed?",
        1,
        |args, _| {
            let name = as_ident_or_str(&args[0], "lsp-installed?")?;
            let installed = with_editor_mut(|st| st.lsp_installed(&name));
            Ok(Rc::new(installed.into()))
        },
        "(lsp-installed?/1)\ntrue when the cache or $PATH holds a binary for the named lsp server.",
    );

    b.be_doc(
        "set-lsp-auto-install",
        1,
        |args, _| {
            let on = args[0].is_truthy();
            with_editor_mut(|st| st.set_lsp_auto_install(on));
            Ok(unit())
        },
        "(set-lsp-auto-install/1)\ntoggle automatic install of lsp servers on file open.\nwhen on (the default), opening a file whose extension matches an lsp.toml entry shells out to the install recipe if the binary is missing.",
    );

    b.be_doc(
        "lsp-auto-install?",
        0,
        |_, _| {
            let on = with_editor_mut(|st| st.lsp_auto_install());
            Ok(Rc::new(on.into()))
        },
        "(lsp-auto-install?/0)\ntrue when lsp servers are auto-installed on first file open (the default).",
    );

    // ---- key-bindable wrappers ---------------------------------------

    b.be_doc(
        "lsp-hover",
        0,
        |_, _| {
            with_editor_mut(|st| {
                let _ = st.apply(&[Rc::new(Action::LspHover)]);
            });
            Ok(unit())
        },
        "(lsp-hover/0)\nrequest textDocument/hover at the cursor and show the result in a notify.",
    );

    b.be_doc(
        "lsp-goto-definition",
        0,
        |_, _| {
            with_editor_mut(|st| {
                let _ = st.apply(&[Rc::new(Action::LspGotoDefinition)]);
            });
            Ok(unit())
        },
        "(lsp-goto-definition/0)\nrequest textDocument/definition at the cursor and jump to the first location.",
    );

    b.be_doc(
        "lsp-completion",
        0,
        |_, _| {
            with_editor_mut(|st| {
                let _ = st.apply(&[Rc::new(Action::LspCompletion)]);
            });
            Ok(unit())
        },
        "(lsp-completion/0)\nrequest textDocument/completion at the cursor and surface the first item.",
    );

    b.be_doc(
        "lsp-format",
        0,
        |_, _| {
            with_editor_mut(|st| {
                let _ = st.apply(&[Rc::new(Action::LspFormat)]);
            });
            Ok(unit())
        },
        "(lsp-format/0)\nrequest textDocument/formatting and apply the edits as one tracked changetree node.",
    );

    b.be_doc(
        "lsp-code-action",
        0,
        |_, _| {
            with_editor_mut(|st| {
                let _ = st.apply(&[Rc::new(Action::LspCodeAction)]);
            });
            Ok(unit())
        },
        "(lsp-code-action/0)\nrequest textDocument/codeAction at the cursor and surface the first action.",
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
        "(lsp-restart [name])\nshut down a running lsp client and re-attach the focused buffer.\nwith no argument, restarts the server for the focused buffer's language.",
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
            )))
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
