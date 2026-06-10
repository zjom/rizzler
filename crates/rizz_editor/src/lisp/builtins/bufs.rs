//! Buffer lifecycle builtins: create / delete / cycle / edit / write.

use rizz_actions::Action;

use super::super::helpers::{Builtins, apply, as_str, unit};

pub(super) fn register(b: &mut Builtins) {
    b.be_doc(
        "buf-create",
        0,
        |_, _| {
            apply(Action::BufCreate {
                set_active: true,
                path: None,
            })?;
            Ok(unit())
        },
        "(buf-create)\n\nCreates a fresh empty buffer and focuses it. Aliased as (bc).\nSee also: (edit PATH), (buf-delete).",
    );
    b.alias("bc", "buf-create");
    b.be_doc(
        "buf-delete",
        0,
        |_, _| {
            apply(Action::BufDelete)?;
            Ok(unit())
        },
        "(buf-delete)\n\nCloses the focused buffer, discarding it from the buffer list. Aliased\nas (bd).\nSee also: (buf-create), (write).",
    );
    b.alias("bd", "buf-delete");
    b.be_doc(
        "buf-next",
        0,
        |_, _| {
            apply(Action::BufNext)?;
            Ok(unit())
        },
        "(buf-next)\n\nFocuses the next buffer in the buffer list, wrapping around. Aliased as\n(bn).\nSee also: (buf-prev).",
    );
    b.alias("bn", "buf-next");
    b.be_doc(
        "buf-prev",
        0,
        |_, _| {
            apply(Action::BufPrev)?;
            Ok(unit())
        },
        "(buf-prev)\n\nFocuses the previous buffer in the buffer list, wrapping around.\nAliased as (bp).\nSee also: (buf-next).",
    );
    b.alias("bp", "buf-prev");
    b.be_doc(
        "edit",
        1,
        |args, _| {
            let p = as_str(&args[0], "edit")?;
            let path = std::path::PathBuf::from(&*p);
            apply(Action::BufEdit(path.into()))?;
            Ok(unit())
        },
        "(edit PATH)\n\nOpens the file at PATH into a buffer and focuses it, reusing the\nexisting buffer if the file is already open. Aliased as (e).\n\nPATH — path: file to open.\nSee also: (write), (write-as PATH).",
    );
    b.alias("e", "edit");
    b.be_doc(
        "write",
        0,
        |_, _| {
            apply(Action::BufWrite(None))?;
            Ok(unit())
        },
        "(write)\n\nWrites the focused buffer to its backing file. Aliased as (w).\nSee also: (write-as PATH), (edit PATH).",
    );
    b.alias("w", "write");
    b.be_doc(
        "write-as",
        1,
        |args, _| {
            let p = as_str(&args[0], "write-as")?;
            let path = std::path::PathBuf::from(&*p);
            apply(Action::BufWrite(Some(path.into())))?;
            Ok(unit())
        },
        "(write-as PATH)\n\nWrites the focused buffer to PATH (vim `:w {file}`), leaving the buffer\nassociated with its original file.\n\nPATH — path: destination file.\nSee also: (write).",
    );
}
