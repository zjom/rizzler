use std::rc::Rc;

use anyhow::anyhow;
use rizz::runtime::{RuntimeError, Value};

use super::super::helpers::{Builtins, as_str, unit};
use super::super::with_editor_mut;

pub(super) fn register(b: &mut Builtins) {
    // Runtime-loaded tree-sitter grammars. `(grammar-register name lib-path
    // scm-path ext)` opens the shared library, resolves its
    // `tree_sitter_<name>` factory, compiles the highlights query, and
    // indexes it by `ext` — either a single string like `".py"` or an array
    // of strings.
    b.be_doc(
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
        "(grammar-register/4)\nregister a tree-sitter grammar loaded from a shared library (.so/.dylib/.dll).\nthe library must export `tree_sitter_<name>` — Neovim's `parser/*.so` ABI.\nargs: <name str> <library-path str> <highlights.scm path str> <ext: str | [str ...]>",
    );
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
