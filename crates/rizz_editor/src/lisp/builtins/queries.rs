//! Read-only buffer/cursor/viewport query builtins surfaced to lisp.

use std::rc::Rc;

use anyhow::anyhow;
use rizz::runtime::{RuntimeError, Value};

use super::super::helpers::{Builtins, as_int, as_usize, buf_id_from_int, buf_id_to_int, unit};
use super::super::with_editor_mut;

pub(super) fn register(b: &mut Builtins) {
    b.be_doc(
        "buffer-text-set",
        2,
        |args, _| {
            let raw = as_int(&args[0], "buffer-text-set")?;
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
        },
        "(buffer-text-set BUFNO TEXT)\n\nReplaces the entire contents of buffer BUFNO with TEXT. Used to drive\npopup buffers from lisp (e.g. seeding a `:messages` popup).\n\nBUFNO — bufno: target buffer, from (buffer-no), (popup-bufno NAME), etc.\nTEXT  — str: the new contents.\n\nErrors when no live buffer has id BUFNO.\nSee also: (buffer-text), (popup-bufno NAME).",
    );

    b.be_doc(
        "buffer-text",
        0,
        |_, _| {
            let s = with_editor_mut(|st| st.focused_buf().text());
            Ok(Rc::new(s.into()))
        },
        "(buffer-text)\n\nReturns str: the full text of the focused buffer.\nSee also: (buffer-text-set BUFNO TEXT), (selected-text), (line-at N).",
    );

    b.be_doc(
        "buffer-nlines",
        0,
        |_, _| {
            let n = with_editor_mut(|st| st.focused_buf().len_lines() as i64);
            Ok(Rc::new(n.into()))
        },
        "(buffer-nlines)\n\nReturns int: the number of lines in the focused buffer.\nSee also: (line-at N), (cursor-line).",
    );

    b.be_doc(
        "buffer-no",
        0,
        |_, _| {
            let id = with_editor_mut(|st| st.focused_buf_id());
            Ok(Rc::new(Value::Int(buf_id_to_int(id))))
        },
        "(buffer-no)\n\nReturns bufno: the opaque id of the focused buffer, for feeding\n(w-buffer-view BUFNO) or (buffer-text-set BUFNO ...).\nSee also: (buffer-index), (popup-bufno NAME), (minibuffer-bufno).",
    );

    b.be_doc(
        "buffer-index",
        0,
        |_, _| {
            let v = with_editor_mut(|st| {
                let id = st.focused_buf_id();
                st.buf_display_index(id)
                    .map(|n| Value::Int(n as i64))
                    .unwrap_or(Value::Unit)
            });
            Ok(Rc::new(v))
        },
        "(buffer-index)\n\nReturns int: the focused buffer's 0-based position in the file buffer\nlist (its order in the bufferline), or () if it has no slot.\nSee also: (buffer-no).",
    );

    b.be_doc(
        "buffer-path",
        0,
        |_, _| {
            let v: Value = with_editor_mut(|st| st.focused_buf().fs_path())
                .map(|p| p.to_string_lossy().as_ref().into())
                .map(|s: Rc<str>| Value::Str(s))
                .unwrap_or(Value::Unit);
            Ok(Rc::new(v))
        },
        "(buffer-path)\n\nReturns str: the filesystem path backing the focused buffer, or () for\nan unsaved scratch buffer. Aliased as (%).\nSee also: (workdir), (edit PATH).",
    );
    b.alias("%", "buffer-path");

    b.be_doc(
        "selected-text",
        0,
        |_, _| {
            let s = with_editor_mut(|st| st.focused_buf().selected_text());
            Ok(Rc::new(s.into()))
        },
        "(selected-text)\n\nReturns str: the text covered by the active visual selection, or () if\nnothing is selected.\nSee also: (buffer-text), (selection-size).",
    );

    b.be_doc(
        "selection-size",
        0,
        |_, _| {
            let n = with_editor_mut(|st| st.focused_buf().selection_size());
            Ok(Rc::new(n.map(|n| n as i64).into()))
        },
        "(selection-size)\n\nReturns int: the char count of the active visual selection, or () if\nnothing is selected. Unlike (len (selected-text)) this never\nmaterializes the selection text, so it's safe to call every frame from\na status line or badge.\nSee also: (selected-text).",
    );

    b.be_doc(
        "cursor-line",
        0,
        |_, _| {
            let n = with_editor_mut(|st| st.focused_buf().abs_row() as i64);
            Ok(Rc::new(n.into()))
        },
        "(cursor-line)\n\nReturns int: the cursor's row in the focused buffer, absolute and\n0-indexed (counts from the top of the buffer, not the viewport).\nSee also: (cursor-col), (cursor-screen-row).",
    );

    b.be_doc(
        "line-at",
        1,
        |args, _| {
            let idx = as_usize(&args[0], "line-at")?;
            let s =
                with_editor_mut(|st| st.focused_buf().lines_at(idx).next().map(|s| s.to_string()));
            Ok(Rc::new(s.into()))
        },
        "(line-at N)\n\nReturns str: the text of line N (0-indexed) in the focused buffer, or ()\nif N is past the end.\n\nN — int: line number.\nSee also: (buffer-nlines), (cursor-line).",
    );

    b.be_doc(
        "cursor-col",
        0,
        |_, _| {
            let n = with_editor_mut(|st| st.focused_buf().abs_col() as i64);
            Ok(Rc::new(n.into()))
        },
        "(cursor-col)\n\nReturns int: the cursor's column in the focused buffer, absolute and\n0-indexed.\nSee also: (cursor-line), (cursor-screen-col).",
    );

    b.be_doc(
        "cursor-screen-row",
        0,
        |_, _| {
            let n = with_editor_mut(|st| st.focused_buf().cursor_pos().row as i64);
            Ok(Rc::new(n.into()))
        },
        "(cursor-screen-row)\n\nReturns int: the cursor's row within its viewport, 0-indexed from the\ntop of the visible region. Pair with (viewport-rows) to decide whether a\npopup fits below the cursor.\nSee also: (cursor-line), (cursor-screen-col), (viewport-rows).",
    );

    b.be_doc(
        "cursor-screen-col",
        0,
        |_, _| {
            let n = with_editor_mut(|st| st.focused_buf().cursor_pos().col as i64);
            Ok(Rc::new(n.into()))
        },
        "(cursor-screen-col)\n\nReturns int: the cursor's column within its viewport, 0-indexed from the\nleft. Useful for positioning cursor-anchored popups.\nSee also: (cursor-col), (cursor-screen-row), (viewport-cols).",
    );

    b.be_doc(
        "viewport-rows",
        0,
        |_, _| {
            let n = with_editor_mut(|st| st.focused_buf().viewport.row as i64);
            Ok(Rc::new(n.into()))
        },
        "(viewport-rows)\n\nReturns int: the focused buffer's visible viewport height in cells.\nSee also: (viewport-cols), (cursor-screen-row).",
    );

    b.be_doc(
        "viewport-cols",
        0,
        |_, _| {
            let n = with_editor_mut(|st| st.focused_buf().viewport.col as i64);
            Ok(Rc::new(n.into()))
        },
        "(viewport-cols)\n\nReturns int: the focused buffer's visible viewport width in cells.\nSee also: (viewport-rows), (cursor-screen-col).",
    );
}
