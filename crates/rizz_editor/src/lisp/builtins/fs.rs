use std::path::PathBuf;
use std::process;
use std::rc::Rc;
use std::str::FromStr;

use anyhow::anyhow;
use im::{HashMap as ImHashMap, Vector};
use rizz::runtime::{RuntimeError, Value};

use super::super::helpers::{Builtins, as_str, unit};

pub(super) fn register(b: &mut Builtins) {
    b.be("fs-canonicalize", 1, |args, _| {
        let s = as_str(&args[0], "fs-canonicalize")?;
        let path = std::fs::canonicalize(s.as_ref())?;
        Ok(Rc::new(path.into()))
    });

    b.be("fs-parent", 1, |args, _| {
        let s = as_str(&args[0], "fs-parent")?;
        let path = PathBuf::from_str(&s).unwrap();
        if let Some(parent) = path.parent()
            && parent.exists()
        {
            Ok(Rc::new(parent.into()))
        } else {
            Ok(unit())
        }
    });

    b.be("fs-readdir", 1, |args, _| {
        let path = as_str(&args[0], "fs-readdir")?;
        let dirs = std::fs::read_dir(path.as_ref())?
            .map(|res| res.map(|e| e.path().into()))
            .collect::<Result<Vector<Value>, std::io::Error>>()?;
        Ok(Rc::new(dirs.into()))
    });
    b.alias("ls", "fs-readdir");
    b.alias("readdir", "fs-readdir");

    b.be("fs-isdir", 1, |args, _| {
        let path = as_str(&args[0], "fs-isdir")?;
        let meta = std::fs::metadata(path.as_ref())?;
        Ok(Rc::new(meta.is_dir().into()))
    });

    b.be("exec", 1, |args, _| {
        let cmd_args = as_str(&args[0], "exec")?;
        let mut prog = cmd_args.split_ascii_whitespace();

        let cmd = prog.next().unwrap_or("");
        if cmd.is_empty() {
            return Err(RuntimeError::type_mismatch(
                "exec",
                "non-empty string",
                &args[0],
            ));
        }
        let output = process::Command::new(cmd).args(prog).output()?;
        let stderr = String::from_utf8(output.stderr).map_err(|e| anyhow!(e))?;
        let stdout = String::from_utf8(output.stdout).map_err(|e| anyhow!(e))?;
        let code = output
            .status
            .code()
            .map(|c| Value::Int(c as i64))
            .unwrap_or(Value::Unit);

        let m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::from_iter([
            (
                Rc::new("success?".into()),
                Rc::new(output.status.success().into()),
            ),
            (Rc::new("stdout".into()), Rc::new(stdout.into())),
            (Rc::new("stderr".into()), Rc::new(stderr.into())),
            (Rc::new("code".into()), Rc::new(code)),
        ]);
        Ok(Rc::new(Value::Map(m)))
    });
}
