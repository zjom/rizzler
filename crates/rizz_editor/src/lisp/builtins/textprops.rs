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
    b.be_doc(
        "put-text-property",
        5,
        |args, _| {
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
        },
        "(put-text-property START-ROW START-COL END-ROW END-COL FACE)\n\nTags the half-open range from (START-ROW, START-COL) to (END-ROW,\nEND-COL) in the focused buffer with FACE. Text properties are static\nstyling, recomputed on edit — for styling you mutate later use\n(overlay-create).\n\nSTART-ROW START-COL END-ROW END-COL — int: 0-indexed range bounds.\nFACE — face: a face name or inline style.\nSee also: (clear-text-properties), (overlay-create ...).",
    );
    b.be_doc(
        "clear-text-properties",
        0,
        |_, _| {
            with_editor_mut(|st| {
                st.focused_buf_mut().props_mut().clear_text_properties();
            });
            Ok(unit())
        },
        "(clear-text-properties)\n\nRemoves every text property from the focused buffer. Leaves overlays\nuntouched.\nSee also: (put-text-property ...), (overlay-delete ID).",
    );

    b.be_doc(
        "overlay-create",
        5,
        |args, _| {
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
        },
        "(overlay-create START-ROW START-COL END-ROW END-COL FACE)\n\nCreates a mutable overlay tagging the half-open range with FACE, and\nreturns its id. Unlike a text property, an overlay can be updated\n((overlay-put)) or removed ((overlay-delete)) after creation.\n\nSTART-ROW START-COL END-ROW END-COL — int: 0-indexed range bounds.\nFACE — face: a face name or inline style.\n\nReturns int: the overlay id, for (overlay-put) / (overlay-delete).\nSee also: (overlay-put ID KEY VALUE), (overlay-delete ID).",
    );
    b.be_doc(
        "overlay-put",
        3,
        |args, _| {
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
        },
        "(overlay-put ID KEY VALUE)\n\nUpdates one property of the overlay ID. A no-op if no overlay has that\nid.\n\nID  — int: an overlay id from (overlay-create).\nKEY — ident | str: which property to set:\n        \"face\"         — VALUE is a face\n        \"priority\"     — VALUE is an int; higher wins on overlap\n        \"pad-to-width\" — VALUE truthy to pad the range to full width\n        \"display\"      — VALUE is a display spec (str | {\"text\": ...}\n                         | {\"space\": N} | ())\n\nErrors when KEY is none of those.\nSee also: (overlay-create ...), (overlay-delete ID).",
    );
    b.be_doc(
        "overlay-delete",
        1,
        |args, _| {
            let id = rizz_text::OverlayId(as_int(&args[0], "overlay-delete")? as u64);
            with_editor_mut(|st| {
                st.focused_buf_mut().props_mut().delete_overlay(id);
            });
            Ok(unit())
        },
        "(overlay-delete ID)\n\nRemoves the overlay ID from the focused buffer. A no-op if no overlay\nhas that id.\n\nID — int: an overlay id from (overlay-create).\nSee also: (overlay-create ...), (clear-text-properties).",
    );
}
