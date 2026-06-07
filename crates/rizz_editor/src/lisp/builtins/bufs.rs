use std::str::FromStr;

use rizz_actions::Action;

use super::super::helpers::{Builtins, apply, as_str, unit};

pub(super) fn register(b: &mut Builtins) {
    b.be("buf-create", 0, |_, _| {
        apply(Action::BufCreate {
            set_active: true,
            path: None,
        })?;
        Ok(unit())
    });
    b.alias("bc", "buf-create");
    b.be("buf-delete", 0, |_, _| {
        apply(Action::BufDelete)?;
        Ok(unit())
    });
    b.alias("bd", "buf-delete");
    b.be("buf-next", 0, |_, _| {
        apply(Action::BufNext)?;
        Ok(unit())
    });
    b.alias("bn", "buf-next");
    b.be("buf-prev", 0, |_, _| {
        apply(Action::BufPrev)?;
        Ok(unit())
    });
    b.alias("bp", "buf-prev");
    b.be("edit", 1, |args, _| {
        let p = as_str(&args[0], "edit")?;
        let path = std::path::PathBuf::from_str(&p).unwrap();
        apply(Action::BufEdit(path.into()))?;
        Ok(unit())
    });
    b.alias("e", "edit");
    b.be("write", 0, |_, _| {
        apply(Action::BufWrite(None))?;
        Ok(unit())
    });
    b.alias("w", "write");
    b.be("write-as", 1, |args, _| {
        let p = as_str(&args[0], "write-as")?;
        let path = std::path::PathBuf::from_str(&p).unwrap();
        apply(Action::BufWrite(Some(path.into())))?;
        Ok(unit())
    });
}
