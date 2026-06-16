//! Top-level editor lifecycle builtins: `quit`, `set-mode`.

use rizz_actions::Action;

use super::super::helpers::{Builtins, apply, parse_mode_ident, unit};

pub(super) fn register(b: &mut Builtins) {
    b.be_doc(
        "quit",
        0,
        |_, _| {
            apply(Action::Quit { force: false })?;
            Ok(unit())
        },
        "(quit)\n\nCloses the focused buffer; exits the application when it is the last\nfile buffer (vim `:q`). Refuses when the buffer has unsaved changes —\nuse (quit!) to discard them. Aliased as (q).\nSee also: (quit!), (quit-all), (write).",
    );
    b.alias("q", "quit");
    b.be_doc(
        "quit!",
        0,
        |_, _| {
            apply(Action::Quit { force: true })?;
            Ok(unit())
        },
        "(quit!)\n\nLike (quit) but discards unsaved changes in the focused buffer (vim\n`:q!`). Aliased as (q!).\nSee also: (quit), (quit-all!).",
    );
    b.alias("q!", "quit!");
    b.be_doc(
        "quit-all",
        0,
        |_, _| {
            apply(Action::QuitAll { force: false })?;
            Ok(unit())
        },
        "(quit-all)\n\nExits the application (vim `:qa`). Refuses when any file buffer has\nunsaved changes — use (quit-all!) to discard them. Aliased as (qa).\nSee also: (quit), (quit-all!).",
    );
    b.alias("qa", "quit-all");
    b.be_doc(
        "quit-all!",
        0,
        |_, _| {
            apply(Action::QuitAll { force: true })?;
            Ok(unit())
        },
        "(quit-all!)\n\nLike (quit-all) but discards unsaved changes in every buffer (vim\n`:qa!`). Aliased as (qa!).\nSee also: (quit-all), (quit!).",
    );
    b.alias("qa!", "quit-all!");

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
