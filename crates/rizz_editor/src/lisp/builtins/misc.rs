//! Catch-all builtins: focused mode/key queries, workdir, config-dir,
//! `reload-config`.

use std::path::PathBuf;
use std::rc::Rc;

use anyhow::anyhow;
use rizz::runtime::{RuntimeError, Value};

use super::super::helpers::{Builtins, unit};
use super::super::with_editor_mut;

pub(super) fn register(b: &mut Builtins) {
    b.be_doc(
        "focused-mode",
        0,
        |_, _| {
            let s = with_editor_mut(|st| st.focused_buf().mode().as_str());
            Ok(Rc::new(Value::Str(s.into())))
        },
        "(focused-mode)\n\nReturns str: the editing mode of the focused buffer (\"normal\",\n\"insert\", \"visual\", …). Drives mode-dependent status lines.\nSee also: (set-mode MODE), (last-key).",
    );

    b.be_doc(
        "last-key",
        0,
        |_, _| {
            let s = with_editor_mut(|st| {
                st.last_key()
                    .map(|k| k.code.to_string())
                    .unwrap_or_else(|| "None".to_string())
            });
            Ok(Rc::new(Value::Str(s.into())))
        },
        "(last-key)\n\nReturns str: a rendering of the most recently pressed key, or \"None\" if\nnone has been seen. Useful for a which-key style status display.\nSee also: (focused-mode).",
    );

    b.be_doc(
        "workdir",
        0,
        |_, _| {
            let d: Value = with_editor_mut(|st| st.workdir()).as_ref().into();
            Ok(Rc::new(d))
        },
        "(workdir)\n\nReturns str: the editor's current working directory, the root that\nrelative paths and (fs-readdir) resolve against.\nSee also: (config-dir), (buf-path).",
    );

    b.be_doc(
        "config-dir",
        0,
        |_, _| {
            let d: Value = with_editor_mut(|st| st.config_dir()).as_ref().into();
            Ok(Rc::new(d))
        },
        "(config-dir)\n\nReturns str: the directory holding init.rz, where (reload-config) reads\nfrom and (open) resolves bare config-relative paths.\nSee also: (workdir), (reload-config).",
    );

    b.bi_doc(
        "reload-config",
        0,
        |_, env| {
            // Read source + capture config dir under the editor borrow,
            // then drop it before eval so editor builtins can be re-entered.
            let (src, dir) =
                with_editor_mut(|st| st.load_init_script().map(|src| (src, st.config_dir())))
                    .map_err(|e| RuntimeError::Other(anyhow!("{e}")))?;
            let prev_basedir = env.base_dir().map(PathBuf::from);
            let eval_env = env.clone().with_base_dir(Some(dir.as_ref().to_path_buf()));
            let (_, new_env) = rizz::parse_and_run_with_env(src.as_bytes(), &eval_env)
                .map_err(|e| RuntimeError::Other(anyhow!("{e}")))?;
            Ok((unit(), new_env.with_base_dir(prev_basedir)))
        },
        "(reload-config)\n\nRe-reads init.rz from the config dir and evaluates it in the running\nenv, so config edits take effect without a restart.\n\nErrors when init.rz cannot be read or raises during evaluation.\nSee also: (config-dir), (evaluate).",
    );
}
