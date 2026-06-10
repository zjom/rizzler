//! Native string helpers for render-path lisp. The gutter fn runs once per
//! visible row per frame and the picker filters thousands of candidates per
//! keystroke, so these hot loops must not run interpreted.

use std::rc::Rc;

use im::Vector;
use rizz::runtime::{RuntimeError, Value};

use super::super::helpers::{Builtins, as_str, as_usize};

pub(super) fn register(b: &mut Builtins) {
    b.be_doc(
        "fuzzy-filter",
        3,
        |args, _| {
            let query = as_str(&args[0], "fuzzy-filter")?;
            let items = args[1].as_array().ok_or_else(|| {
                RuntimeError::type_mismatch("fuzzy-filter", "array", &args[1])
            })?;
            let key = match &*args[2] {
                Value::Unit => None,
                Value::Str(s) | Value::Ident(s) => Some(s.clone()),
                _ => {
                    return Err(RuntimeError::type_mismatch(
                        "fuzzy-filter",
                        "str|()",
                        &args[2],
                    ));
                }
            };
            let needle: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
            let key = key.map(|k| Rc::new(Value::Str(k)));
            let out: Vector<Rc<Value>> = items
                .iter()
                .filter(|it| {
                    let hay = match key.as_ref() {
                        Some(k) => match &***it {
                            Value::Map(m) => m.get(k).and_then(|v| v.as_str()),
                            _ => None,
                        },
                        None => it.as_str(),
                    };
                    hay.is_some_and(|h| subsequence_match(&needle, &h))
                })
                .cloned()
                .collect();
            Ok(Rc::new(Value::Array(out)))
        },
        "(fuzzy-filter QUERY ITEMS KEY)\n\nReturns array: the ITEMS whose haystack contains every char of QUERY in\norder (case-insensitive subsequence match, gaps allowed), in source\norder. An empty QUERY passes everything. Native — filtering thousands\nof candidates per keystroke stays cheap.\n\nQUERY — str: the typed filter text.\nITEMS — array: maps (haystack under KEY) or plain strings (KEY = ()).\nKEY   — str: map key holding the haystack, or () for string items.\nItems without a string haystack are dropped.\nSee also: (longest-common-prefix STRS).",
    );

    b.be_doc(
        "str-pad-left",
        2,
        |args, _| {
            let s = as_str(&args[0], "str-pad-left")?;
            let w = as_usize(&args[1], "str-pad-left")?;
            Ok(Rc::new(Value::Str(pad(&s, w, true).into())))
        },
        "(str-pad-left S W)\n\nReturns str: S left-padded with spaces to W chars. S is returned\nunchanged when it is already W chars or longer.\n\nS — str: the text to pad.\nW — int: the target width in chars.\nSee also: (str-pad-right S W), (str-repeat S N).",
    );

    b.be_doc(
        "str-pad-right",
        2,
        |args, _| {
            let s = as_str(&args[0], "str-pad-right")?;
            let w = as_usize(&args[1], "str-pad-right")?;
            Ok(Rc::new(Value::Str(pad(&s, w, false).into())))
        },
        "(str-pad-right S W)\n\nReturns str: S right-padded with spaces to W chars. S is returned\nunchanged when it is already W chars or longer.\n\nS — str: the text to pad.\nW — int: the target width in chars.\nSee also: (str-pad-left S W), (str-repeat S N).",
    );

    b.be_doc(
        "str-repeat",
        2,
        |args, _| {
            let s = as_str(&args[0], "str-repeat")?;
            let n = as_usize(&args[1], "str-repeat")?;
            Ok(Rc::new(Value::Str(s.repeat(n).into())))
        },
        "(str-repeat S N)\n\nReturns str: S concatenated N times (\"\" when N is 0).\n\nS — str: the text to repeat.\nN — int: the repetition count.\nSee also: (str-pad-left S W), (str-pad-right S W).",
    );
}

/// Are all of `needle`'s chars found in `hay`, in order, gaps allowed?
/// `needle` is pre-lowercased; `hay` is lowercased on the fly.
fn subsequence_match(needle: &[char], hay: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let mut i = 0;
    for h in hay.chars().flat_map(char::to_lowercase) {
        if h == needle[i] {
            i += 1;
            if i == needle.len() {
                return true;
            }
        }
    }
    false
}

fn pad(s: &str, w: usize, left: bool) -> String {
    let len = s.chars().count();
    if len >= w {
        return s.to_string();
    }
    let fill = " ".repeat(w - len);
    if left {
        format!("{fill}{s}")
    } else {
        format!("{s}{fill}")
    }
}
