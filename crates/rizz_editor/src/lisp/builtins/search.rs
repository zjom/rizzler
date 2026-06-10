//! Lisp surface for `/`-style search. Each builtin is a thin shim that
//! emits the matching [`rizz_actions::Action`] so the real work lives in
//! `State::apply` (single-funnel invariant).

use rizz_actions::Action;

use super::super::helpers::{Builtins, apply, unit};

pub(super) fn register(b: &mut Builtins) {
    b.be_doc(
        "search-submit",
        0,
        |_, _| {
            apply(Action::SearchSubmit)?;
            Ok(unit())
        },
        "(search-submit)\n\nCommits the query typed into the search minibuffer and jumps to the\nfirst match. Bound to Enter in the search prompt.\nSee also: (search-next), (search-cancel).",
    );
    b.be_doc(
        "search-cancel",
        0,
        |_, _| {
            apply(Action::SearchCancel)?;
            Ok(unit())
        },
        "(search-cancel)\n\nDismisses the search prompt and returns the cursor to where the search\nstarted.\nSee also: (search-submit).",
    );
    b.be_doc(
        "search-next",
        0,
        |_, _| {
            apply(Action::SearchNext)?;
            Ok(unit())
        },
        "(search-next)\n\nMoves to the next match of the active search (vim `n`), wrapping around\nthe buffer.\nSee also: (search-prev), (search-submit).",
    );
    b.be_doc(
        "search-prev",
        0,
        |_, _| {
            apply(Action::SearchPrev)?;
            Ok(unit())
        },
        "(search-prev)\n\nMoves to the previous match of the active search (vim `N`), wrapping\naround the buffer.\nSee also: (search-next).",
    );
}
