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
        "(grammar-register NAME LIB-PATH SCM-PATH EXTS)\n\nRegisters a tree-sitter grammar loaded from a pre-built shared library.\nAn escape hatch — (grammar-install NAME) is the curated wrapper most\nconfigs want.\n\nNAME     — str: the grammar name; the library must export\n           `tree_sitter_<NAME>` (Neovim's parser/*.so ABI).\nLIB-PATH — path: the .so / .dylib / .dll to load.\nSCM-PATH — path: a highlights.scm query file, read at registration.\nEXTS     — str | array of str: file extension(s) to index by.\n\nErrors when the library or query file can't be loaded.\nSee also: (grammar-install NAME), (grammar-installed? NAME).",
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
        r#"(grammar-install NAME [OPTS])

Installs a tree-sitter grammar by NAME from the curated grammars.toml
manifest: clones via git, builds via the `tree-sitter` CLI, and caches
the result. Idempotent on cache hit.

NAME — ident | str: the grammar name.
OPTS — map: optional overrides. Recognized keys:
         "repo":       str — git URL (overrides the manifest)
         "path":       path — use a local checkout instead of cloning
         "branch":     str — git branch to clone
         "rev":        str — git ref to check out
         "subdir":     str — subdir where parser.c lives
         "extensions": array of str — file extensions to index by
         "language":   str — override the tree_sitter_<…> symbol suffix
         "queries":    str — highlights.scm path, relative to source root
         "force":      truthy — rebuild even if the cache stamp matches

Returns a pair: (ok . NAME) on success, or (err . MESSAGE) on failure.

Errors are returned in the result pair rather than raised; the process
still needs `git` and `tree-sitter` on $PATH.
See also: (grammar-installed? NAME), (set-grammar-auto-install ON),
(grammar-register ...)."#,
    );

    b.be_doc(
        "grammar-installed?",
        1,
        |args, _| {
            let name = as_ident_or_str(&args[0], "grammar-installed?")?;
            let installed = with_editor_mut(|st| st.grammar_installed(&name));
            Ok(Rc::new(installed.into()))
        },
        "(grammar-installed? NAME)\n\nReturns 1 if the local cache holds a parser library and highlights query\nfor NAME, else 0. Purely local — never touches the network.\n\nNAME — ident | str: the grammar name.\nSee also: (grammar-install NAME).",
    );

    b.be_doc(
        "set-grammar-auto-install",
        1,
        |args, _| {
            let on = args[0].is_truthy();
            with_editor_mut(|st| st.set_grammar_auto_install(on));
            Ok(unit())
        },
        "(set-grammar-auto-install ON)\n\nToggles automatic grammar install on file open. When ON (the default),\nopening a file whose extension matches a grammars.toml entry shells out\nto `git` + `tree-sitter` once to install and cache the grammar. When off,\na missing grammar surfaces a one-time notify pointing at\n(grammar-install '<name>) instead.\n\nON — int: nonzero to enable, 0 to disable.\nSee also: (grammar-auto-install?), (grammar-install NAME).",
    );

    b.be_doc(
        "grammar-auto-install?",
        0,
        |_, _| {
            let on = with_editor_mut(|st| st.grammar_auto_install());
            Ok(Rc::new(on.into()))
        },
        "(grammar-auto-install?)\n\nReturns 1 if grammars are auto-installed on first file open (the\ndefault), else 0.\nSee also: (set-grammar-auto-install ON).",
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
