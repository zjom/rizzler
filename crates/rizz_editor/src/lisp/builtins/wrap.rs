//! Soft-wrap mode / wrap-column / breakindent builtins.

use std::rc::Rc;

use rizz::runtime::Value;
use rizz_text::wrap::WrapMode;

use super::super::helpers::{Builtins, as_ident_or_str, as_int, unit, unknown_variant};
use super::super::with_editor_mut;

pub(super) fn register(b: &mut Builtins) {
    b.be("buffer-wrap", 0, |args, _| {
        if let Some(arg) = args.first() {
            let sym = as_ident_or_str(arg, "buffer-wrap")?;
            let m = WrapMode::from_str(&sym).ok_or_else(|| unknown_variant("buffer-wrap", &sym))?;
            with_editor_mut(|st| st.focused_buf_mut().set_wrap_mode(m));
            Ok(unit())
        } else {
            let s: Rc<str> = with_editor_mut(|st| st.focused_buf().wrap_mode().as_str().into());
            Ok(Rc::new(Value::Str(s)))
        }
    });
    b.be("buffer-wrap?", 0, |_, _| {
        let s: Rc<str> = with_editor_mut(|st| st.focused_buf().wrap_mode().as_str().into());
        Ok(Rc::new(Value::Str(s)))
    });

    b.be("buffer-wrap-column", 1, |args, _| {
        let n = as_int(&args[0], "buffer-wrap-column")?;
        let col = if n <= 0 {
            None
        } else {
            Some(n.min(u16::MAX as i64) as u16)
        };
        with_editor_mut(|st| st.focused_buf_mut().set_wrap_column(col));
        Ok(unit())
    });

    b.be("buffer-breakindent", 1, |args, _| {
        let n = as_int(&args[0], "buffer-breakindent")?;
        with_editor_mut(|st| st.focused_buf_mut().set_breakindent(n != 0));
        Ok(unit())
    });
}
