//! Text-property and overlay builtins: tag ranges with face/display props
//! and manage mutable overlays.

use std::rc::Rc;

use rizz::runtime::{RuntimeError, Value};
use rizz_core::Position;
use rizz_text::props::PropEntry;

use super::super::helpers::{
    Builtins, as_ident_or_str, as_int, as_usize, display_from_value, unit,
};
use super::super::with_editor_mut;

pub(super) fn register(b: &mut Builtins) {
    b.be("put-text-property", 5, |args, _| {
        let start_row = as_usize(&args[0], "put-text-property")?;
        let start_col = as_usize(&args[1], "put-text-property")?;
        let end_row = as_usize(&args[2], "put-text-property")?;
        let end_col = as_usize(&args[3], "put-text-property")?;
        let face = args[4].clone();
        with_editor_mut(|st| {
            st.focused_buf_mut()
                .props_mut()
                .push_text_property(PropEntry {
                    start: Position::new(start_col, start_row),
                    end: Position::new(end_col, end_row),
                    face: Some(face),
                    display: None,
                    priority: 0,
                    pad_to_width: false,
                });
        });
        Ok(unit())
    });
    b.be("clear-text-properties", 0, |_, _| {
        with_editor_mut(|st| {
            st.focused_buf_mut().props_mut().clear_text_properties();
        });
        Ok(unit())
    });

    b.be("overlay-create", 5, |args, _| {
        let start_row = as_usize(&args[0], "overlay-create")?;
        let start_col = as_usize(&args[1], "overlay-create")?;
        let end_row = as_usize(&args[2], "overlay-create")?;
        let end_col = as_usize(&args[3], "overlay-create")?;
        let face = args[4].clone();
        let id = with_editor_mut(|st| {
            st.focused_buf_mut().props_mut().create_overlay(PropEntry {
                start: Position::new(start_col, start_row),
                end: Position::new(end_col, end_row),
                face: Some(face),
                display: None,
                priority: 0,
                pad_to_width: false,
            })
        });
        Ok(Rc::new(Value::Int(id.0 as i64)))
    });
    b.be("overlay-put", 3, |args, _| {
        let id = rizz_text::OverlayId(as_int(&args[0], "overlay-put")? as u64);
        let key = as_ident_or_str(&args[1], "overlay-put")?;
        enum Update {
            Face(Rc<Value>),
            Priority(i64),
            PadToWidth(bool),
            Display(Option<rizz_core::Display>),
        }
        let update = match key.as_ref() {
            "face" => Update::Face(args[2].clone()),
            "priority" => Update::Priority(as_int(&args[2], "overlay-put")?),
            "pad-to-width" => Update::PadToWidth(args[2].is_truthy()),
            "display" => Update::Display(display_from_value(&args[2])?),
            other => {
                return Err(RuntimeError::TypeMismatch {
                    name: "overlay-put".into(),
                    expected: "face|priority|pad-to-width|display".into(),
                    got: other.into(),
                });
            }
        };
        with_editor_mut(|st| {
            if let Some(e) = st.focused_buf_mut().props_mut().overlay_mut(id) {
                match update {
                    Update::Face(f) => e.face = Some(f),
                    Update::Priority(p) => e.priority = p,
                    Update::PadToWidth(b) => e.pad_to_width = b,
                    Update::Display(d) => e.display = d,
                }
            }
        });
        Ok(unit())
    });
    b.be("overlay-delete", 1, |args, _| {
        let id = rizz_text::OverlayId(as_int(&args[0], "overlay-delete")? as u64);
        with_editor_mut(|st| {
            st.focused_buf_mut().props_mut().delete_overlay(id);
        });
        Ok(unit())
    });
}
