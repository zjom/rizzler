//! Lisp surface for the editor's vim-style registers.
//!
//! Read/write the registers directly (`register-read`, `register-write`),
//! stage the next consuming action's target (`register-select`), or run the
//! standard yank/paste actions through the keymap funnel (`yank`,
//! `yank-line`, `yank-motion`, `paste`, `paste-before`). `(registers)`
//! returns a map for introspection, used by an optional `:reg` popup in
//! `init.rz`.

use std::rc::Rc;
use std::str::FromStr;

use im::HashMap as ImHashMap;
use rizz::runtime::{RuntimeError, Value};
use rizz_actions::Action;
use rizz_registers::{RegisterEntry, RegisterKind};
use rizz_text::{MoveKind, TextObject};

use super::super::helpers::{
    Builtins, apply, as_ident, as_str, str_mismatch, unit, unknown_variant,
};
use super::super::with_editor_mut;

/// Extract a single ASCII register name from a lisp string, e.g. `"a"`,
/// `"\""`, `"0"`. Empty or multi-char strings are rejected.
fn as_register_name(v: &Rc<Value>, name: &str) -> Result<char, RuntimeError> {
    let s = as_str(v, name)?;
    let mut chars = s.chars();
    let c = chars
        .next()
        .ok_or_else(|| str_mismatch(name, "single-char string"))?;
    if chars.next().is_some() {
        return Err(str_mismatch(name, "single-char string"));
    }
    Ok(c)
}

fn kind_from_symbol(v: Option<&Rc<Value>>) -> Result<RegisterKind, RuntimeError> {
    let Some(v) = v else {
        return Ok(RegisterKind::Char);
    };
    let s = as_ident(v, "register-kind")?;
    Ok(match s.as_ref() {
        "char" | "charwise" => RegisterKind::Char,
        "line" | "linewise" => RegisterKind::Line,
        "block" | "blockwise" => RegisterKind::Block,
        _ => return Err(unknown_variant("register-kind", &s)),
    })
}

fn kind_symbol(kind: RegisterKind) -> Rc<Value> {
    let s: Rc<str> = match kind {
        RegisterKind::Char => "char".into(),
        RegisterKind::Line => "line".into(),
        RegisterKind::Block => "block".into(),
    };
    Rc::new(Value::Ident(s))
}

pub(super) fn register(b: &mut Builtins) {
    b.be("yank", 0, |_, _| {
        apply(Action::YankSelection)?;
        Ok(unit())
    });

    b.be("yank-line", 0, |_, _| {
        let count = with_editor_mut(|st| st.pending_count_or_one());
        apply(Action::YankLine { count })?;
        Ok(unit())
    });

    b.be("yank-motion", 1, |args, _| {
        let sym = as_ident(&args[0], "yank-motion")?;
        let kind = MoveKind::from_str(&sym).map_err(|_| unknown_variant("yank-motion", &sym))?;
        let count = with_editor_mut(|st| st.pending_count_or_one());
        apply(Action::YankMotion { kind, count })?;
        Ok(unit())
    });

    b.be("paste", 0, |_, _| {
        let count = with_editor_mut(|st| st.pending_count_or_one());
        apply(Action::Paste {
            before: false,
            count,
        })?;
        Ok(unit())
    });

    b.be("paste-before", 0, |_, _| {
        let count = with_editor_mut(|st| st.pending_count_or_one());
        apply(Action::Paste {
            before: true,
            count,
        })?;
        Ok(unit())
    });

    b.be("register-select", 1, |args, _| {
        let name = as_register_name(&args[0], "register-select")?;
        apply(Action::RegisterSelect(name))?;
        Ok(unit())
    });

    b.be("register-read", 1, |args, _| {
        let name = as_register_name(&args[0], "register-read")?;
        let v = with_editor_mut(|st| match st.registers().read(name) {
            Some(entry) => Value::Str(entry.text.clone()),
            None => Value::Unit,
        });
        Ok(Rc::new(v))
    });

    b.be("register-kind", 1, |args, _| {
        let name = as_register_name(&args[0], "register-kind")?;
        let v = with_editor_mut(|st| {
            st.registers()
                .read(name)
                .map(|e| kind_symbol(e.kind))
                .unwrap_or_else(unit)
        });
        Ok(v)
    });

    b.be("register-write", 2, |args, _| {
        let name = as_register_name(&args[0], "register-write")?;
        let text = as_str(&args[1], "register-write")?;
        apply(Action::RegisterSet {
            name,
            text,
            kind: RegisterKind::Char,
        })?;
        Ok(unit())
    });

    // (register-write-linewise "a" "abc\n") — explicit kind override.
    b.be("register-write-linewise", 2, |args, _| {
        let name = as_register_name(&args[0], "register-write-linewise")?;
        let text = as_str(&args[1], "register-write-linewise")?;
        apply(Action::RegisterSet {
            name,
            text,
            kind: RegisterKind::Line,
        })?;
        Ok(unit())
    });

    b.be("registers", 0, |_, _| {
        let m = with_editor_mut(|st| {
            let mut m: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
            for (name, entry) in st.registers().iter() {
                let key: Rc<Value> = Rc::new(Value::Str(name.to_string().into()));
                let mut row: ImHashMap<Rc<Value>, Rc<Value>> = ImHashMap::new();
                row.insert(
                    Rc::new(Value::Str("text".into())),
                    Rc::new(Value::Str(entry.text.clone())),
                );
                row.insert(Rc::new(Value::Str("kind".into())), kind_symbol(entry.kind));
                m.insert(key, Rc::new(Value::Map(row)));
            }
            Value::Map(m)
        });
        Ok(Rc::new(m))
    });

    b.be("register-pending", 0, |_, _| {
        let v = with_editor_mut(|st| match st.pending_register() {
            Some(c) => Value::Str(c.to_string().into()),
            None => Value::Unit,
        });
        Ok(Rc::new(v))
    });

    // Set a register's contents using a kind symbol — useful for restoring
    // a register from a (registers) snapshot or for plumbing block-mode text.
    b.be("register-set", 3, |args, _| {
        let name = as_register_name(&args[0], "register-set")?;
        let text = as_str(&args[1], "register-set")?;
        let kind = kind_from_symbol(Some(&args[2]))?;
        apply(Action::RegisterSet { name, text, kind })?;
        Ok(unit())
    });

    // Direct, non-action register write — does not record an action, used
    // by tests / introspection helpers that want to seed a register without
    // going through `apply`. Same kind defaults as `register-write`.
    b.be("register-poke", 2, |args, _| {
        let name = as_register_name(&args[0], "register-poke")?;
        let text = as_str(&args[1], "register-poke")?;
        with_editor_mut(|st| {
            st.registers_mut()
                .write(name, RegisterEntry::charwise(text.clone()));
        });
        Ok(unit())
    });

    // ---- text objects (vim's `i<x>` / `a<x>`) ---------------------------
    //
    // Three operators × {around, inner}. Each takes a single object symbol
    // (`'word`, `'big-word`, `'paren`, `'bracket`, `'brace`, `'angle`,
    // `'double-quote`, `'single-quote`, `'backtick`, or the corresponding
    // single-char string `"("` / `"\""` / etc.). The pending count prefix
    // is forwarded — `2daw` deletes the second-outer word, `2di(` expands
    // to the second-outer paren pair.

    b.be("yank-inner", 1, |args, _| {
        apply_text_object(&args[0], "yank-inner", |object, count| {
            Action::YankTextObject {
                object,
                around: false,
                count,
            }
        })?;
        Ok(unit())
    });

    b.be("yank-around", 1, |args, _| {
        apply_text_object(&args[0], "yank-around", |object, count| {
            Action::YankTextObject {
                object,
                around: true,
                count,
            }
        })?;
        Ok(unit())
    });

    b.be("delete-inner", 1, |args, _| {
        apply_text_object(&args[0], "delete-inner", |object, count| {
            Action::DeleteTextObject {
                object,
                around: false,
                count,
            }
        })?;
        Ok(unit())
    });

    b.be("delete-around", 1, |args, _| {
        apply_text_object(&args[0], "delete-around", |object, count| {
            Action::DeleteTextObject {
                object,
                around: true,
                count,
            }
        })?;
        Ok(unit())
    });

    b.be("select-inner", 1, |args, _| {
        apply_text_object(&args[0], "select-inner", |object, count| {
            Action::SelectTextObject {
                object,
                around: false,
                count,
            }
        })?;
        Ok(unit())
    });

    b.be("select-around", 1, |args, _| {
        apply_text_object(&args[0], "select-around", |object, count| {
            Action::SelectTextObject {
                object,
                around: true,
                count,
            }
        })?;
        Ok(unit())
    });
}

/// Parse the text-object arg (symbol or single-char string), grab the
/// pending count prefix, and apply the action `build` constructs from them.
fn apply_text_object(
    arg: &Rc<Value>,
    name: &'static str,
    build: impl FnOnce(TextObject, u32) -> Action,
) -> Result<(), RuntimeError> {
    let sym = match &**arg {
        Value::Ident(s) | Value::Str(s) => s.clone(),
        _ => return Err(RuntimeError::type_mismatch(name, "ident|str", arg)),
    };
    let object = TextObject::from_str(&sym).map_err(|_| unknown_variant(name, &sym))?;
    let count = with_editor_mut(|st| st.pending_count_or_one());
    apply(build(object, count))
}

