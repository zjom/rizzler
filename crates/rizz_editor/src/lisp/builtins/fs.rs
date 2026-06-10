//! Filesystem and subprocess builtins (canonicalize, readdir, exec, …).

use std::path::PathBuf;
use std::process;
use std::rc::Rc;

use anyhow::anyhow;
use im::{HashMap as ImHashMap, Vector};
use rizz::runtime::{RuntimeError, Value};

use super::super::helpers::{Builtins, as_str, unit};

pub(super) fn register(b: &mut Builtins) {
    b.be_doc(
        "fs-canonicalize",
        1,
        |args, _| {
            let s = as_str(&args[0], "fs-canonicalize")?;
            let path = std::fs::canonicalize(s.as_ref())?;
            Ok(Rc::new(path.into()))
        },
        "(fs-canonicalize PATH)\n\nReturns str: PATH resolved to an absolute path with symlinks followed.\n\nPATH — path: the path to resolve.\n\nErrors when PATH does not exist.\nSee also: (fs-parent PATH), (fs-readdir PATH).",
    );

    b.be_doc(
        "fs-parent",
        1,
        |args, _| {
            let s = as_str(&args[0], "fs-parent")?;
            let path = PathBuf::from(&*s);
            if let Some(parent) = path.parent()
                && parent.exists()
            {
                Ok(Rc::new(parent.into()))
            } else {
                Ok(unit())
            }
        },
        "(fs-parent PATH)\n\nReturns str: the parent directory of PATH, or () if PATH has no parent\nor the parent doesn't exist.\n\nPATH — path: the path whose parent to take.\nSee also: (fs-canonicalize PATH).",
    );

    b.be_doc(
        "fs-readdir",
        1,
        |args, _| {
            let path = as_str(&args[0], "fs-readdir")?;
            let dirs = std::fs::read_dir(path.as_ref())?
                .map(|res| res.map(|e| e.path().into()))
                .collect::<Result<Vector<Value>, std::io::Error>>()?;
            Ok(Rc::new(dirs.into()))
        },
        "(fs-readdir PATH)\n\nReturns array of str: the paths of the entries directly inside directory\nPATH, in filesystem order. Aliased as (ls) and (readdir).\n\nPATH — path: the directory to list.\n\nErrors when PATH is not a readable directory.\nSee also: (fs-isdir PATH), (fs-parent PATH).",
    );
    b.alias("ls", "fs-readdir");
    b.alias("readdir", "fs-readdir");

    b.be_doc(
        "fs-isdir",
        1,
        |args, _| {
            let path = as_str(&args[0], "fs-isdir")?;
            let meta = std::fs::metadata(path.as_ref())?;
            Ok(Rc::new(meta.is_dir().into()))
        },
        "(fs-isdir PATH)\n\nReturns 1 if PATH is a directory, else 0.\n\nPATH — path: the path to test.\n\nErrors when PATH does not exist.\nSee also: (fs-readdir PATH).",
    );

    b.be_doc(
        "exec",
        1,
        |args, _| {
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
        },
        "(exec CMDLINE)\n\nRuns CMDLINE as a subprocess, waits for it, and returns its result.\nCMDLINE is split on ASCII whitespace — the first token is the program,\nthe rest are arguments. There is no shell, so quoting, globs, and pipes\nare not interpreted.\n\nCMDLINE — str: the command and its arguments.\n\nReturns map: {\"success?\": 1|0, \"stdout\": str, \"stderr\": str,\n\"code\": int | ()}. \"code\" is () when the process was killed by a signal.\n\nErrors when CMDLINE is empty, the program can't be spawned, or its\noutput is not valid UTF-8.\n\nExample:\n  (get (exec \"git rev-parse --short HEAD\") \"stdout\")",
    );
}
