//! Theme/face builtins: define named faces, look them up, build rgb values.

use std::rc::Rc;

use rizz::runtime::Value;
use rizz_ui::styling::{rgb_value, style_from_value, style_to_value};

use super::super::helpers::{Builtins, as_ident_or_str, as_u8, unit};
use super::super::with_editor_mut;

pub(super) fn register(b: &mut Builtins) {
    b.be_doc(
        "face-define",
        2,
        |args, _| {
            let name = as_ident_or_str(&args[0], "face-define")?;
            let style = with_editor_mut(|st| {
                let theme = st.theme().borrow();
                style_from_value(&args[1], &theme)
            })?;
            with_editor_mut(|st| {
                st.theme().borrow_mut().insert(name, style);
            });
            Ok(unit())
        },
        "(face-define NAME STYLE)\n\nDefines or replaces the named theme face NAME with STYLE, so later\nwidgets and text properties can refer to it by name.\n\nNAME  — ident | str: the face name.\nSTYLE — style: a base face name, or an inline map with keys fg, bg,\n        bold, italic, underline, reverse, inherit. Colors are color\n        names or (rgb R G B) values.\n\nExample:\n  (face-define 'header {\"fg\": (rgb 200 200 40) \"bold\": 1})\nSee also: (face-of NAME), (rgb R G B).",
    );
    b.be_doc(
        "face-of",
        1,
        |args, _| {
            let name = as_ident_or_str(&args[0], "face-of")?;
            let v = with_editor_mut(|st| {
                st.theme()
                    .borrow()
                    .lookup(&name)
                    .map(style_to_value)
                    .unwrap_or_else(|| Rc::new(Value::Unit))
            });
            Ok(v)
        },
        "(face-of NAME)\n\nReturns map: the resolved style of the theme face NAME (its fg/bg/\nattributes), or () if no such face is defined.\n\nNAME — ident | str: the face name.\nSee also: (face-define NAME STYLE).",
    );
    b.be_doc(
        "rgb",
        3,
        |args, _| {
            let r = as_u8(&args[0], "rgb")?;
            let g = as_u8(&args[1], "rgb")?;
            let b_ = as_u8(&args[2], "rgb")?;
            Ok(rgb_value(r, g, b_))
        },
        "(rgb R G B)\n\nReturns color: a true-color value built from the three channels, for\nuse anywhere a color is expected (face fg/bg).\n\nR G B — int: channel values, each clamped to 0..=255.\n\nExample:\n  (face-define 'accent {\"fg\": (rgb 255 90 90)})\nSee also: (face-define NAME STYLE).",
    );
}
