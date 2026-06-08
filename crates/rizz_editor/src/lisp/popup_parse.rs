//! Parsers that translate the `(popup-show ...)` options map into a
//! [`PopupSpec`]. Split out from the builtin itself so the lisp/builtins
//! tree only handles dispatch.

use std::rc::Rc;

use rizz::runtime::{RuntimeError, Value};

use rizz_text::wrap::WrapMode;
use rizz_ui::panel::parse_placement;

use super::helpers::{
    as_ident_or_str, as_int, as_str, parse_mode_ident, parse_mode_layers, parse_mode_name,
};
use crate::state::PopupSpec;

pub(super) fn parse_popup_options(v: &Rc<Value>, spec: &mut PopupSpec) -> Result<(), RuntimeError> {
    let m = match &**v {
        Value::Unit => return Ok(()),
        Value::Map(m) => m,
        _ => {
            return Err(RuntimeError::type_mismatch(
                "popup-show.options",
                "map | ()",
                v,
            ));
        }
    };
    let key = |k: &str| Rc::new(Value::Str(k.into()));
    if let Some(t) = m.get(&key("text")) {
        spec.initial_text = Some(as_str(t, "popup-show.text")?.to_string());
    }
    if let Some(modes) = m.get(&key("modes")) {
        spec.mode_layers = parse_mode_layers(modes)?;
    } else if let Some(mode) = m.get(&key("mode")) {
        spec.mode_layers = vec![parse_mode_name(mode)?];
    }
    if let Some(bm) = m.get(&key("buffer-mode")) {
        spec.buffer_mode = parse_mode_ident(bm)?;
    }
    if let Some(p) = m.get(&key("placement")) {
        spec.placement = parse_placement(p)?;
    }
    if let Some(sc) = m.get(&key("show-cursor")) {
        spec.show_cursor = sc.is_truthy();
    }
    if let Some(sc) = m.get(&key("wrap-mode")) {
        spec.wrap_mode =
            WrapMode::from_str(&as_ident_or_str(sc, "popup-show.wrap-mode")?).unwrap_or_default();
    }
    if let Some(sc) = m.get(&key("wrap-column")) {
        spec.wrap_column = Some(as_int(sc, "popup-show.wrap-column")?.max(0) as u16);
    }
    if let Some(sc) = m.get(&key("break-indent")) {
        spec.breakindent = sc.is_truthy();
    }
    Ok(())
}
