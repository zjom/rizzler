//! Native string helpers for render-path lisp. The gutter fn runs once per
//! visible row per frame, so its padding helpers must not be built from
//! interpreted per-char loops.

use std::rc::Rc;

use rizz::runtime::Value;

use super::super::helpers::{Builtins, as_str, as_usize};

pub(super) fn register(b: &mut Builtins) {
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
