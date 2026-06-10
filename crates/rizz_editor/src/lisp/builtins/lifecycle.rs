//! Top-level editor lifecycle builtins: `quit`, `set-mode`.

use rizz_actions::Action;

use super::super::helpers::{Builtins, apply, parse_mode_ident, unit};

pub(super) fn register(b: &mut Builtins) {
    b.be_doc(
        "quit",
        0,
        |_, _| {
            apply(Action::Quit)?;
            Ok(unit())
        },
        "(quit)\n\nExits the application. Aliased as (q).",
    );
    b.alias("q", "quit");

    b.be_doc(
        "set-mode",
        1,
        |args, _| {
            let mode = parse_mode_ident(&args[0])?;
            apply(Action::SetMode(mode))?;
            Ok(unit())
        },
        "(set-mode MODE)\n\nSwitches the focused buffer to editing mode MODE.\n\nMODE — mode: one of 'normal 'insert 'visual 'visual-line\n       'visual-block 'command.\n\nErrors when MODE is not one of those idents.\nSee also: (focused-mode).",
    );
}
