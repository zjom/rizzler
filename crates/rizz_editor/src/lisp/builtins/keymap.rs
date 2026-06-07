use std::rc::Rc;

use im::HashMap as ImHashMap;
use rizz::runtime::Value;
use rizz_actions::Action;
use rizz_input::KeyEvent;

use super::super::helpers::{Builtins, apply, as_str, parse_mode_name, str_mismatch_msg, unit};
use super::super::with_editor_mut;

pub(super) fn register(b: &mut Builtins) {
    b.be("keymap-set", 3, |args, _| {
        let mode = parse_mode_name(&args[0])?;
        let lhs_str = as_str(&args[1], "keymap-set")?;
        let lhs =
            KeyEvent::parse_sequence(&lhs_str).map_err(|e| str_mismatch_msg("keymap-set", &e))?;
        let form = args[2].clone();
        apply(Action::KeymapSet {
            mode,
            lhs,
            rhs: Rc::new(Action::EvalLisp(form)),
        })?;
        Ok(unit())
    });

    b.be("keymap-remove", 2, |args, _| {
        let mode = parse_mode_name(&args[0])?;
        let lhs_str = as_str(&args[1], "keymap-remove")?;
        let lhs = KeyEvent::parse_sequence(&lhs_str)
            .map_err(|e| str_mismatch_msg("keymap-remove", &e))?;
        apply(Action::KeymapRemove { mode, lhs })?;
        Ok(unit())
    });

    b.be("keymap-get", 1, |args, _| {
        let mode = parse_mode_name(&args[0])?;
        let mappings = with_editor_mut(|st| {
            st.keymap_registry()
                .iter()
                .filter(|(m, _, _)| m == &mode)
                .map(|(m, p, a)| {
                    let lhs: String = p.iter().map(|e| e.to_string()).collect::<Vec<_>>().concat();
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
    });
}
