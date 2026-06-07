use std::rc::Rc;

use anyhow::anyhow;
use rizz::runtime::{RuntimeError, Value};

use super::super::helpers::{Builtins, as_int, as_usize, unit};
use super::super::with_editor_mut;

pub(super) fn register(b: &mut Builtins) {
    b.be("buf-text-set", 2, |args, _| {
        let bufno = as_int(&args[0], "buf-text-set")?;
        if bufno < 0 {
            return Err(RuntimeError::type_mismatch(
                "buf-text-set",
                "integer >= 0",
                &args[0],
            ));
        }
        let text = args[1].display();
        let nbufs = with_editor_mut(|st| st.nbufs());
        if bufno as usize >= nbufs {
            return Err(RuntimeError::Other(anyhow!(
                "bad input. editor has {nbufs} 0-indexed buffers but you requested buffer {bufno}"
            )));
        }

        with_editor_mut(|st| st.set_buffer_contents(bufno as usize, &text));
        Ok(unit())
    });

    b.be("buf-text", 0, |_, _| {
        let s = with_editor_mut(|st| st.focused_buf().text());
        Ok(Rc::new(s.into()))
    });

    b.be("buf-no", 0, |_, _| {
        let s = with_editor_mut(|st| st.focused_bufno());
        Ok(Rc::new(Value::Int(s as i64)))
    });

    b.be("buf-path", 0, |_, _| {
        let v: Value = with_editor_mut(|st| st.focused_buf().fs_path())
            .map(|p| p.to_string_lossy().as_ref().into())
            .map(|s: Rc<str>| Value::Str(s))
            .unwrap_or(Value::Unit);
        Ok(Rc::new(v))
    });
    b.alias("%", "buf-path");

    b.be("selected-text", 0, |_, _| {
        let s = with_editor_mut(|st| st.focused_buf().selected_text());
        Ok(Rc::new(s.into()))
    });

    b.be("cursor-line", 0, |_, _| {
        let n = with_editor_mut(|st| st.focused_buf().abs_row() as i64);
        Ok(Rc::new(n.into()))
    });

    b.be("line-at", 1, |args, _| {
        let idx = as_usize(&args[0], "line-at")?;
        let s = with_editor_mut(|st| st.focused_buf().lines_at(idx).next().map(|s| s.to_string()));
        Ok(Rc::new(s.into()))
    });

    b.be("cursor-col", 0, |_, _| {
        let n = with_editor_mut(|st| st.focused_buf().abs_col() as i64);
        Ok(Rc::new(n.into()))
    });
}
