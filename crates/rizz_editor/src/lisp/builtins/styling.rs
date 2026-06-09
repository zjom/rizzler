//! Theme/face builtins: define named faces, look them up, build rgb values.

use std::rc::Rc;

use rizz::runtime::Value;
use rizz_ui::styling::{rgb_value, style_from_value, style_to_value};

use super::super::helpers::{Builtins, as_ident_or_str, as_u8, unit};
use super::super::with_editor_mut;

pub(super) fn register(b: &mut Builtins) {
    b.be("face-define", 2, |args, _| {
        let name = as_ident_or_str(&args[0], "face-define")?;
        let style = with_editor_mut(|st| {
            let theme = st.theme().borrow();
            style_from_value(&args[1], &theme)
        })?;
        with_editor_mut(|st| {
            st.theme().borrow_mut().insert(name, style);
        });
        Ok(unit())
    });
    b.be("face-of", 1, |args, _| {
        let name = as_ident_or_str(&args[0], "face-of")?;
        let v = with_editor_mut(|st| {
            st.theme()
                .borrow()
                .lookup(&name)
                .map(style_to_value)
                .unwrap_or_else(|| Rc::new(Value::Unit))
        });
        Ok(v)
    });
    b.be("rgb", 3, |args, _| {
        let r = as_u8(&args[0], "rgb")?;
        let g = as_u8(&args[1], "rgb")?;
        let b_ = as_u8(&args[2], "rgb")?;
        Ok(rgb_value(r, g, b_))
    });
}
