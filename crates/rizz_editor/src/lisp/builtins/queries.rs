use std::rc::Rc;

use anyhow::anyhow;
use rizz::runtime::{RuntimeError, Value};

use super::super::helpers::{Builtins, as_int, as_usize, buf_id_from_int, buf_id_to_int, unit};
use super::super::with_editor_mut;

pub(super) fn register(b: &mut Builtins) {
    b.be("buf-text-set", 2, |args, _| {
        let raw = as_int(&args[0], "buf-text-set")?;
        let id = buf_id_from_int(raw);
        let text = args[1].display();
        let exists = with_editor_mut(|st| st.buf_exists(id));
        if !exists {
            return Err(RuntimeError::Other(anyhow!(
                "bad input. no buffer with id {raw}"
            )));
        }
        with_editor_mut(|st| st.set_buffer_contents(id, &text));
        Ok(unit())
    });

    b.be("buf-text", 0, |_, _| {
        let s = with_editor_mut(|st| st.focused_buf().text());
        Ok(Rc::new(s.into()))
    });

    b.be("buf-nlines", 0, |_, _| {
        let n = with_editor_mut(|st| st.focused_buf().len_lines() as i64);
        Ok(Rc::new(n.into()))
    });

    b.be("buf-no", 0, |_, _| {
        let id = with_editor_mut(|st| st.focused_buf_id());
        Ok(Rc::new(Value::Int(buf_id_to_int(id))))
    });

    b.be("buf-index", 0, |_, _| {
        let v = with_editor_mut(|st| {
            let id = st.focused_buf_id();
            st.buf_display_index(id)
                .map(|n| Value::Int(n as i64))
                .unwrap_or(Value::Unit)
        });
        Ok(Rc::new(v))
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

    b.be_doc(
        "cursor-screen-row",
        0,
        |_, _| {
            let n = with_editor_mut(|st| st.focused_buf().cursor_pos().row as i64);
            Ok(Rc::new(n.into()))
        },
        "(cursor-screen-row/0)\nthe focused buffer's cursor row within its viewport (0-indexed from\nthe top of the visible region). pair with (viewport-rows) to decide\nwhether a popup fits below the cursor.",
    );

    b.be_doc(
        "cursor-screen-col",
        0,
        |_, _| {
            let n = with_editor_mut(|st| st.focused_buf().cursor_pos().col as i64);
            Ok(Rc::new(n.into()))
        },
        "(cursor-screen-col/0)\nthe focused buffer's cursor column within its viewport (0-indexed\nfrom the left). useful for positioning cursor-anchored popups.",
    );

    b.be_doc(
        "viewport-rows",
        0,
        |_, _| {
            let n = with_editor_mut(|st| st.focused_buf().viewport.row as i64);
            Ok(Rc::new(n.into()))
        },
        "(viewport-rows/0)\nthe focused buffer's visible viewport height in cells.",
    );

    b.be_doc(
        "viewport-cols",
        0,
        |_, _| {
            let n = with_editor_mut(|st| st.focused_buf().viewport.col as i64);
            Ok(Rc::new(n.into()))
        },
        "(viewport-cols/0)\nthe focused buffer's visible viewport width in cells.",
    );
}
