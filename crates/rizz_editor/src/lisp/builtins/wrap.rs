//! Soft-wrap mode / wrap-column / breakindent builtins.

use std::rc::Rc;

use rizz::runtime::Value;
use rizz_text::wrap::WrapMode;

use super::super::helpers::{Builtins, as_ident_or_str, as_int, unit, unknown_variant};
use super::super::with_editor_mut;

pub(super) fn register(b: &mut Builtins) {
    b.be_doc(
        "buffer-wrap",
        0,
        |args, _| {
            if let Some(arg) = args.first() {
                let sym = as_ident_or_str(arg, "buffer-wrap")?;
                let m =
                    WrapMode::parse(&sym).ok_or_else(|| unknown_variant("buffer-wrap", &sym))?;
                with_editor_mut(|st| st.focused_buf_mut().set_wrap_mode(m));
                Ok(unit())
            } else {
                let s: Rc<str> = with_editor_mut(|st| st.focused_buf().wrap_mode().as_str().into());
                Ok(Rc::new(Value::Str(s)))
            }
        },
        "(buffer-wrap [MODE])\n\nGets or sets the focused buffer's soft-wrap mode. With no argument,\nreturns str: the current mode. With MODE, sets it.\n\nMODE — ident | str: 'none (no wrap), 'char (wrap mid-word), or 'word\n       (wrap at word boundaries).\n\nErrors when MODE is none of those.\nSee also: (buffer-wrap-column N), (buffer-breakindent ON).",
    );
    b.be_doc(
        "buffer-wrap?",
        0,
        |_, _| {
            let s: Rc<str> = with_editor_mut(|st| st.focused_buf().wrap_mode().as_str().into());
            Ok(Rc::new(Value::Str(s)))
        },
        "(buffer-wrap?)\n\nReturns str: the focused buffer's current soft-wrap mode ('none, 'char,\nor 'word). The read-only counterpart to (buffer-wrap MODE).\nSee also: (buffer-wrap [MODE]).",
    );

    b.be_doc(
        "buffer-wrap-column",
        1,
        |args, _| {
            let n = as_int(&args[0], "buffer-wrap-column")?;
            let col = if n <= 0 {
                None
            } else {
                Some(n.min(u16::MAX as i64) as u16)
            };
            with_editor_mut(|st| st.focused_buf_mut().set_wrap_column(col));
            Ok(unit())
        },
        "(buffer-wrap-column N)\n\nSets the column the focused buffer wraps at. N <= 0 clears the fixed\ncolumn, wrapping at the viewport edge instead.\n\nN — int: the wrap column, or 0 for viewport-width.\nSee also: (buffer-wrap [MODE]), (buffer-breakindent ON).",
    );

    b.be_doc(
        "buffer-breakindent",
        1,
        |args, _| {
            let n = as_int(&args[0], "buffer-breakindent")?;
            with_editor_mut(|st| st.focused_buf_mut().set_breakindent(n != 0));
            Ok(unit())
        },
        "(buffer-breakindent ON)\n\nToggles breakindent on the focused buffer: when ON is truthy, soft-\nwrapped continuation rows inherit the line's leading indentation.\n\nON — int: nonzero to enable, 0 to disable.\nSee also: (buffer-wrap [MODE]).",
    );
}
