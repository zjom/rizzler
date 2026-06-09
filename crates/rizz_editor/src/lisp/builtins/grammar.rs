//! Tree-sitter grammar registration and install builtins.

use std::path::PathBuf;
use std::rc::Rc;

use anyhow::anyhow;
use rizz::runtime::{RuntimeError, Value};

use rizz_ts_install::InstallOpts;

use super::super::helpers::{Builtins, as_ident_or_str, as_str, unit};
use super::super::with_editor_mut;

pub(super) fn register(b: &mut Builtins) {
    // Escape hatch: register a grammar from a pre-built shared library.
    // `(grammar-install)` is the curated wrapper most users want.
    b.be_doc(
        "grammar-register",
        4,
        |args, _| {
            let name = as_str(&args[0], "grammar-register")?;
            let lib_path = as_str(&args[1], "grammar-register")?;
            let scm_path = as_str(&args[2], "grammar-register")?;
            let exts = parse_extensions(&args[3])?;
            let highlights = std::fs::read_to_string(scm_path.as_ref())?;
            let lib_path = PathBuf::from(lib_path.as_ref());
            with_editor_mut(|st| st.register_grammar(&name, &exts, &lib_path, &highlights))
                .map_err(|e| RuntimeError::Other(anyhow!("{e}")))?;
            Ok(unit())
        },
        "(grammar-register/4)\nregister a tree-sitter grammar loaded from a shared library (.so/.dylib/.dll).\nthe library must export `tree_sitter_<name>` — Neovim's `parser/*.so` ABI.\nargs: <name str> <library-path str> <highlights.scm path str> <ext: str | [str ...]>",
    );

    b.be_doc(
        "grammar-install",
        1,
        |args, _| {
            let name = as_ident_or_str(&args[0], "grammar-install")?;
            let opts = if args.len() >= 2 {
                parse_install_opts(&args[1])?
            } else {
                InstallOpts::default()
            };
            let install_res = with_editor_mut(|st| st.install_grammar(&name, opts));
            Ok(Rc::new(match install_res {
                Ok(_) => Value::Cons {
                    head: Rc::new(Value::Ident("ok".into())),
                    tail: args[0].clone(),
                },
                Err(e) => Value::Cons {
                    head: Rc::new(Value::Ident("err".into())),
                    tail: Rc::new(Value::Str(e.to_string().into())),
                },
            }))
        },
        r#"(grammar-install name opts?)
install a tree-sitter grammar by name from the curated grammars.toml manifest.
clones via git, builds via the `tree-sitter` CLI, and caches the result.
idempotent on cache hit. `git` and `tree-sitter` must be on $PATH.

params:
- `name`: str | ident
- `opts`: map
   recognised keys:
   - "repo":       git URL (overrides manifest)
   - "path":       use a local checkout instead of cloning
   - "branch":     git branch to clone
   - "rev":        git ref to check out
   - "subdir":     subdir inside the repo where parser.c lives
   - "extensions": [str ...] file extensions to index by
   - "language":   override the tree_sitter_<…> C symbol suffix
   - "queries":    override the highlights.scm path (relative to source root)
   - "force":      truthy → rebuild even if the cache stamp matches"#,
    );

    b.be_doc(
        "grammar-installed?",
        1,
        |args, _| {
            let name = as_ident_or_str(&args[0], "grammar-installed?")?;
            let installed = with_editor_mut(|st| st.grammar_installed(&name));
            Ok(Rc::new(installed.into()))
        },
        "(grammar-installed?/1)\ntrue when the local cache holds a parser library + highlights query for <name>.\npurely local — never touches the network.",
    );

    b.be_doc(
        "set-grammar-auto-install",
        1,
        |args, _| {
            let on = args[0].is_truthy();
            with_editor_mut(|st| st.set_grammar_auto_install(on));
            Ok(unit())
        },
        "(set-grammar-auto-install/1)\ntoggle automatic grammar install on file open.\nwhen on (the default), opening a file whose extension matches an entry in grammars.toml shells out\nto `git` + `tree-sitter` once to install + cache the grammar. when off, the missing grammar surfaces\na one-time notify pointing at `(grammar-install '<name>)` instead.",
    );

    b.be_doc(
        "grammar-auto-install?",
        0,
        |_, _| {
            let on = with_editor_mut(|st| st.grammar_auto_install());
            Ok(Rc::new(on.into()))
        },
        "(grammar-auto-install?/0)\ntrue when grammars are auto-installed on first file open (the default).",
    );
}

/// Accept either a single extension string or an array of them. The leading
/// dot is optional.
fn parse_extensions(v: &Rc<Value>) -> Result<Vec<String>, RuntimeError> {
    match &**v {
        Value::Array(items) => items
            .iter()
            .map(|x| as_str(x, "grammar-register.ext").map(|s| s.to_string()))
            .collect(),
        _ => Ok(vec![as_str(v, "grammar-register.ext")?.to_string()]),
    }
}

fn parse_install_opts(v: &Rc<Value>) -> Result<InstallOpts, RuntimeError> {
    let m = match &**v {
        Value::Unit => return Ok(InstallOpts::default()),
        Value::Map(m) => m,
        _ => {
            return Err(RuntimeError::type_mismatch(
                "grammar-install.opts",
                "map | ()",
                v,
            ));
        }
    };
    let key = |k: &str| Rc::new(Value::Str(k.into()));
    let mut opts = InstallOpts::default();
    if let Some(v) = m.get(&key("repo")) {
        opts.repo = Some(as_str(v, "grammar-install.repo")?.to_string());
    }
    if let Some(v) = m.get(&key("path")) {
        opts.path = Some(PathBuf::from(as_str(v, "grammar-install.path")?.as_ref()));
    }
    if let Some(v) = m.get(&key("branch")) {
        opts.branch = Some(as_str(v, "grammar-install.branch")?.to_string());
    }
    if let Some(v) = m.get(&key("rev")) {
        opts.rev = Some(as_str(v, "grammar-install.rev")?.to_string());
    }
    if let Some(v) = m.get(&key("subdir")) {
        opts.subdir = Some(as_str(v, "grammar-install.subdir")?.to_string());
    }
    if let Some(v) = m.get(&key("extensions")) {
        opts.extensions = Some(parse_extensions(v)?);
    }
    if let Some(v) = m.get(&key("language")) {
        opts.language = Some(as_str(v, "grammar-install.language")?.to_string());
    }
    if let Some(v) = m.get(&key("queries")) {
        opts.queries = Some(as_str(v, "grammar-install.queries")?.to_string());
    }
    if let Some(v) = m.get(&key("force")) {
        opts.force = v.is_truthy();
    }
    Ok(opts)
}
