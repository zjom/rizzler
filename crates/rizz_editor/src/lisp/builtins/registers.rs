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

/// Extract a single ASCII register name from a lisp string. Empty or
/// multi-char strings are rejected.
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
    b.be_doc(
        "yank",
        0,
        |_, _| {
            apply(Action::YankSelection)?;
            Ok(unit())
        },
        "(yank)\n\nCopies the active visual selection into the register staged by\n(register-select), or the unnamed register otherwise (vim `y`).\nSee also: (yank-line), (yank-motion KIND), (paste).",
    );

    b.be_doc(
        "yank-line",
        0,
        |_, _| {
            let count = with_editor_mut(|st| st.pending_count_or_one());
            apply(Action::YankLine { count })?;
            Ok(unit())
        },
        "(yank-line)\n\nCopies the current line linewise (vim `yy`), honoring the pending count\nprefix.\nSee also: (yank), (delete-line), (paste).",
    );

    b.be_doc(
        "yank-motion",
        1,
        |args, _| {
            let sym = as_ident(&args[0], "yank-motion")?;
            let kind =
                MoveKind::from_str(&sym).map_err(|_| unknown_variant("yank-motion", &sym))?;
            let count = with_editor_mut(|st| st.pending_count_or_one());
            apply(Action::YankMotion { kind, count })?;
            Ok(unit())
        },
        "(yank-motion KIND)\n\nCopies the text the motion KIND would cover from the cursor (vim `yw`),\nhonoring the pending count prefix.\n\nKIND — move-kind: the motion to sweep, e.g. 'word-next, 'line-end.\n\nErrors when KIND is not a known motion.\nSee also: (delete-motion KIND), (yank).",
    );

    b.be_doc(
        "paste",
        0,
        |_, _| {
            let count = with_editor_mut(|st| st.pending_count_or_one());
            apply(Action::Paste {
                before: false,
                count,
            })?;
            Ok(unit())
        },
        "(paste)\n\nPastes the selected register after the cursor (vim `p`), honoring the\npending count prefix. Linewise registers paste onto the next line.\nSee also: (paste-before), (yank), (register-select REGISTER).",
    );

    b.be_doc(
        "paste-before",
        0,
        |_, _| {
            let count = with_editor_mut(|st| st.pending_count_or_one());
            apply(Action::Paste {
                before: true,
                count,
            })?;
            Ok(unit())
        },
        "(paste-before)\n\nPastes the selected register before the cursor (vim `P`), honoring the\npending count prefix.\nSee also: (paste), (yank).",
    );

    b.be_doc(
        "register-select",
        1,
        |args, _| {
            let name = as_register_name(&args[0], "register-select")?;
            apply(Action::RegisterSelect(name))?;
            Ok(unit())
        },
        "(register-select REGISTER)\n\nStages REGISTER as the target of the next yank/delete/paste, the lisp\nequivalent of vim's `\"a` prefix.\n\nREGISTER — register: a single-character register name.\nSee also: (register-pending), (yank), (paste).",
    );

    b.be_doc(
        "register-read",
        1,
        |args, _| {
            let name = as_register_name(&args[0], "register-read")?;
            let v = with_editor_mut(|st| match st.registers().read(name) {
                Some(entry) => Value::Str(entry.text.clone()),
                None => Value::Unit,
            });
            Ok(Rc::new(v))
        },
        "(register-read REGISTER)\n\nReturns str: the text held in REGISTER, or () if it is empty.\n\nREGISTER — register: a single-character register name.\nSee also: (register-write REGISTER TEXT), (register-kind REGISTER).",
    );

    b.be_doc(
        "register-kind",
        1,
        |args, _| {
            let name = as_register_name(&args[0], "register-kind")?;
            let v = with_editor_mut(|st| {
                st.registers()
                    .read(name)
                    .map(|e| kind_symbol(e.kind))
                    .unwrap_or_else(unit)
            });
            Ok(v)
        },
        "(register-kind REGISTER)\n\nReturns ident: how REGISTER's text pastes — 'char, 'line, or 'block —\nor () if the register is empty.\n\nREGISTER — register: a single-character register name.\nSee also: (register-read REGISTER), (registers).",
    );

    b.be_doc(
        "register-write",
        2,
        |args, _| {
            let name = as_register_name(&args[0], "register-write")?;
            let text = as_str(&args[1], "register-write")?;
            apply(Action::RegisterSet {
                name,
                text,
                kind: RegisterKind::Char,
            })?;
            Ok(unit())
        },
        "(register-write REGISTER TEXT)\n\nStores TEXT into REGISTER charwise, through the action funnel.\n\nREGISTER — register: a single-character register name.\nTEXT     — str: the text to store.\nSee also: (register-write-linewise REGISTER TEXT), (register-set\nREGISTER TEXT KIND), (register-read REGISTER).",
    );

    b.be_doc(
        "register-write-linewise",
        2,
        |args, _| {
            let name = as_register_name(&args[0], "register-write-linewise")?;
            let text = as_str(&args[1], "register-write-linewise")?;
            apply(Action::RegisterSet {
                name,
                text,
                kind: RegisterKind::Line,
            })?;
            Ok(unit())
        },
        "(register-write-linewise REGISTER TEXT)\n\nStores TEXT into REGISTER linewise, so a later (paste) drops it onto its\nown line.\n\nREGISTER — register: a single-character register name.\nTEXT     — str: the text to store.\nSee also: (register-write REGISTER TEXT).",
    );

    b.be_doc(
        "registers",
        0,
        |_, _| {
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
        },
        "(registers)\n\nReturns map: a snapshot of every non-empty register, keyed by its\nsingle-character name. Each value is {\"text\": str, \"kind\": ident}.\nDrives an optional `:reg` popup in init.rz.\nSee also: (register-read REGISTER), (register-pending).",
    );

    b.be_doc(
        "register-pending",
        0,
        |_, _| {
            let v = with_editor_mut(|st| match st.pending_register() {
                Some(c) => Value::Str(c.to_string().into()),
                None => Value::Unit,
            });
            Ok(Rc::new(v))
        },
        "(register-pending)\n\nReturns str: the register staged by (register-select) and awaiting its\nnext consuming action, or () if none is pending.\nSee also: (register-select REGISTER).",
    );

    // Set a register using an explicit kind symbol; lets callers restore
    // a register from a `(registers)` snapshot or plumb block-mode text.
    b.be_doc(
        "register-set",
        3,
        |args, _| {
            let name = as_register_name(&args[0], "register-set")?;
            let text = as_str(&args[1], "register-set")?;
            let kind = kind_from_symbol(Some(&args[2]))?;
            apply(Action::RegisterSet { name, text, kind })?;
            Ok(unit())
        },
        "(register-set REGISTER TEXT KIND)\n\nStores TEXT into REGISTER with an explicit paste KIND. Lets callers\nrestore a register from a (registers) snapshot or plumb block-mode text.\n\nREGISTER — register: a single-character register name.\nTEXT     — str: the text to store.\nKIND     — ident: 'char, 'line, or 'block.\n\nErrors when KIND is none of those idents.\nSee also: (register-write REGISTER TEXT).",
    );

    // Non-action write — seeds a register without going through `apply`.
    // Used by tests and introspection helpers.
    b.be_doc(
        "register-poke",
        2,
        |args, _| {
            let name = as_register_name(&args[0], "register-poke")?;
            let text = as_str(&args[1], "register-poke")?;
            with_editor_mut(|st| {
                st.registers_mut()
                    .write(name, RegisterEntry::charwise(text.clone()));
            });
            Ok(unit())
        },
        "(register-poke REGISTER TEXT)\n\nSeeds REGISTER with charwise TEXT directly, bypassing the action funnel\n(no undo entry). Intended for tests and introspection helpers; prefer\n(register-write) in normal config.\n\nREGISTER — register: a single-character register name.\nTEXT     — str: the text to store.",
    );

    // Text objects (vim's `i<x>` / `a<x>`): three operators × {around,
    // inner}. The pending count prefix is forwarded.
    b.be_doc(
        "yank-inner",
        1,
        |args, _| {
            apply_text_object(&args[0], "yank-inner", |object, count| {
                Action::YankTextObject {
                    object,
                    around: false,
                    count,
                }
            })?;
            Ok(unit())
        },
        "(yank-inner OBJ)\n\nCopies the inner text object OBJ at the cursor (vim `yi{`), honoring the\npending count prefix.\n\nOBJ — text-object: e.g. 'word 'paren 'quote 'brace.\n\nErrors when OBJ is not a known text object.\nSee also: (yank-around OBJ), (delete-inner OBJ), (select-inner OBJ).",
    );

    b.be_doc(
        "yank-around",
        1,
        |args, _| {
            apply_text_object(&args[0], "yank-around", |object, count| {
                Action::YankTextObject {
                    object,
                    around: true,
                    count,
                }
            })?;
            Ok(unit())
        },
        "(yank-around OBJ)\n\nCopies the text object OBJ including its delimiters (vim `ya{`),\nhonoring the pending count prefix.\n\nOBJ — text-object: e.g. 'word 'paren 'quote 'brace.\n\nErrors when OBJ is not a known text object.\nSee also: (yank-inner OBJ), (delete-around OBJ).",
    );

    b.be_doc(
        "delete-inner",
        1,
        |args, _| {
            apply_text_object(&args[0], "delete-inner", |object, count| {
                Action::DeleteTextObject {
                    object,
                    around: false,
                    count,
                }
            })?;
            Ok(unit())
        },
        "(delete-inner OBJ)\n\nDeletes the inner text object OBJ at the cursor (vim `di{`), honoring\nthe pending count prefix.\n\nOBJ — text-object: e.g. 'word 'paren 'quote 'brace.\n\nErrors when OBJ is not a known text object.\nSee also: (delete-around OBJ), (yank-inner OBJ), (select-inner OBJ).",
    );

    b.be_doc(
        "delete-around",
        1,
        |args, _| {
            apply_text_object(&args[0], "delete-around", |object, count| {
                Action::DeleteTextObject {
                    object,
                    around: true,
                    count,
                }
            })?;
            Ok(unit())
        },
        "(delete-around OBJ)\n\nDeletes the text object OBJ including its delimiters (vim `da{`),\nhonoring the pending count prefix.\n\nOBJ — text-object: e.g. 'word 'paren 'quote 'brace.\n\nErrors when OBJ is not a known text object.\nSee also: (delete-inner OBJ), (yank-around OBJ).",
    );

    b.be_doc(
        "select-inner",
        1,
        |args, _| {
            apply_text_object(&args[0], "select-inner", |object, count| {
                Action::SelectTextObject {
                    object,
                    around: false,
                    count,
                }
            })?;
            Ok(unit())
        },
        "(select-inner OBJ)\n\nVisually selects the inner text object OBJ at the cursor (vim `vi{`),\nhonoring the pending count prefix.\n\nOBJ — text-object: e.g. 'word 'paren 'quote 'brace.\n\nErrors when OBJ is not a known text object.\nSee also: (select-around OBJ), (yank-inner OBJ).",
    );

    b.be_doc(
        "select-around",
        1,
        |args, _| {
            apply_text_object(&args[0], "select-around", |object, count| {
                Action::SelectTextObject {
                    object,
                    around: true,
                    count,
                }
            })?;
            Ok(unit())
        },
        "(select-around OBJ)\n\nVisually selects the text object OBJ including its delimiters (vim\n`va{`), honoring the pending count prefix.\n\nOBJ — text-object: e.g. 'word 'paren 'quote 'brace.\n\nErrors when OBJ is not a known text object.\nSee also: (select-inner OBJ), (delete-around OBJ).",
    );
}

/// Parse the text-object arg, grab the pending count prefix, and apply the
/// action `build` constructs from them.
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
