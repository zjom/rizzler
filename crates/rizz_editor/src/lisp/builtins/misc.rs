use std::path::PathBuf;
use std::rc::Rc;

use anyhow::anyhow;
use rizz::runtime::{RuntimeError, Value};

use super::super::helpers::{Builtins, unit};
use super::super::with_editor_mut;

pub(super) fn register(b: &mut Builtins) {
    b.be("focused-mode", 0, |_, _| {
        let s = with_editor_mut(|st| st.focused_buf().mode().as_str());
        Ok(Rc::new(Value::Str(s.into())))
    });

    b.be("last-key", 0, |_, _| {
        let s = with_editor_mut(|st| {
            st.last_key()
                .map(|k| k.code.to_string())
                .unwrap_or_else(|| "None".to_string())
        });
        Ok(Rc::new(Value::Str(s.into())))
    });

    b.be("workdir", 0, |_, _| {
        let d: Value = with_editor_mut(|st| st.workdir()).as_ref().into();
        Ok(Rc::new(d))
    });

    b.be_doc(
        "config-dir",
        0,
        |_, _| {
            let d: Value = with_editor_mut(|st| st.config_dir()).as_ref().into();
            Ok(Rc::new(d))
        },
        "(config-dir/0)\nreturn the directory holding init.rz",
    );

    b.bi_doc(
        "reload-config",
        0,
        |_, env| {
            // Read source + capture config dir under the editor borrow, then
            // drop it before eval so the parser can re-enter editor builtins.
            let (src, dir) =
                with_editor_mut(|st| st.load_init_script().map(|src| (src, st.config_dir())))
                    .map_err(|e| RuntimeError::Other(anyhow!("{e}")))?;
            let prev_basedir = env.base_dir().map(PathBuf::from);
            let eval_env = env.clone().with_base_dir(Some(dir.as_ref().to_path_buf()));
            let (_, new_env) = rizz::parse_and_run_with_env(src.as_bytes(), &eval_env)
                .map_err(|e| RuntimeError::Other(anyhow!("{e}")))?;
            Ok((unit(), new_env.with_base_dir(prev_basedir)))
        },
        "(reload-config/0)\nre-read init.rz from the config dir and evaluate it",
    );
}
