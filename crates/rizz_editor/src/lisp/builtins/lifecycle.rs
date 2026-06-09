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
        "(quit/0)\nexit the application",
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
        "(set-mode/1)\nchange the editing mode.\naccepts one of: 'normal | 'insert | 'visual | 'visual-line | 'visual-block | 'command",
    );
}
