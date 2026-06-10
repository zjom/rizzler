//! Keymap mutation builtins: set/remove/get bindings.

use std::rc::Rc;

use im::HashMap as ImHashMap;
use rizz::runtime::Value;
use rizz_actions::Action;
use rizz_input::KeyEvent;

use super::super::helpers::{Builtins, apply, as_str, parse_mode_name, str_mismatch_msg, unit};
use super::super::with_editor_mut;

pub(super) fn register(b: &mut Builtins) {
    b.be_doc(
        "keymap-set",
        3,
        |args, _| {
            let mode = parse_mode_name(&args[0])?;
            let lhs_str = as_str(&args[1], "keymap-set")?;
            let lhs = KeyEvent::parse_sequence(&lhs_str)
                .map_err(|e| str_mismatch_msg("keymap-set", &e))?;
            let form = args[2].clone();
            apply(Action::KeymapSet {
                mode,
                lhs,
                rhs: Rc::new(Action::EvalLisp(form)),
            })?;
            Ok(unit())
        },
        "(keymap-set LAYER KEYS FORM)\n\nBinds the key sequence KEYS in keymap layer LAYER to run the lisp FORM.\nRe-binding an existing KEYS replaces it.\n\nLAYER — layer: a mode or keymap-layer name, e.g. 'normal, 'insert, or a\n        popup layer like 'popup.\nKEYS  — key-seq: a key sequence, e.g. \"q\", \"C-x\", \"g g\".\nFORM  — the unevaluated form to run when KEYS is pressed (quote it).\n\nErrors when KEYS is not a parseable key sequence.\n\nExample:\n  (keymap-set 'normal \"q\" '(quit))\n  (keymap-set 'popup \"<esc>\" '(popup-close))\nSee also: (keymap-remove LAYER KEYS), (keymap-get LAYER).",
    );

    b.be_doc(
        "keymap-remove",
        2,
        |args, _| {
            let mode = parse_mode_name(&args[0])?;
            let lhs_str = as_str(&args[1], "keymap-remove")?;
            let lhs = KeyEvent::parse_sequence(&lhs_str)
                .map_err(|e| str_mismatch_msg("keymap-remove", &e))?;
            apply(Action::KeymapRemove { mode, lhs })?;
            Ok(unit())
        },
        "(keymap-remove LAYER KEYS)\n\nRemoves the binding for KEYS from keymap layer LAYER. A no-op if nothing\nis bound there.\n\nLAYER — layer: the mode or keymap-layer name.\nKEYS  — key-seq: the key sequence to unbind.\n\nErrors when KEYS is not a parseable key sequence.\nSee also: (keymap-set LAYER KEYS FORM).",
    );

    b.be_doc(
        "keymap-get",
        1,
        |args, _| {
            let mode = parse_mode_name(&args[0])?;
            let mappings = with_editor_mut(|st| {
                st.keymap_registry()
                    .iter()
                    .filter(|(m, _, _)| m == &mode)
                    .map(|(m, p, a)| {
                        let lhs: String =
                            p.iter().map(|e| e.to_string()).collect::<Vec<_>>().concat();
                        let rhs: Value = match a {
                            Action::EvalLisp(form) => (**form).clone(),
                            other => format!("{:?}", other).into(),
                        };
                        let entry: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::from_iter([
                            (Rc::new(Value::Str("mode".into())), Rc::new(Value::Str(m))),
                            (
                                Rc::new(Value::Str("lhs".into())),
                                Rc::new(Value::Str(lhs.into())),
                            ),
                            (Rc::new(Value::Str("rhs".into())), Rc::new(rhs)),
                        ]);
                        Value::Map(entry)
                    })
                    .collect::<Vec<Value>>()
            });
            Ok(Rc::new(mappings.into()))
        },
        "(keymap-get LAYER)\n\nReturns array: every binding in keymap layer LAYER, each a map\n{\"mode\": str, \"lhs\": str, \"rhs\": form}. Drives an optional `:map` popup.\n\nLAYER — layer: the mode or keymap-layer name.\nSee also: (keymap-set LAYER KEYS FORM).",
    );
}
