//! Lisp surface for `/`-style search. Each builtin is a thin shim that
//! emits the matching [`rizz_actions::Action`] so the real work lives in
//! `State::apply` (single-funnel invariant).

use rizz_actions::Action;

use super::super::helpers::{Builtins, apply, unit};

pub(super) fn register(b: &mut Builtins) {
    b.be("search-submit", 0, |_, _| {
        apply(Action::SearchSubmit)?;
        Ok(unit())
    });
    b.be("search-cancel", 0, |_, _| {
        apply(Action::SearchCancel)?;
        Ok(unit())
    });
    b.be("search-next", 0, |_, _| {
        apply(Action::SearchNext)?;
        Ok(unit())
    });
    b.be("search-prev", 0, |_, _| {
        apply(Action::SearchPrev)?;
        Ok(unit())
    });
}
