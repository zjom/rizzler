use rizz_actions::Action;
use rizz_core::{FocusDir, SplitDir};

use super::super::helpers::{Builtins, apply, as_ident, unit, unknown_variant};

pub(super) fn register(b: &mut Builtins) {
    b.be("window-split", 1, |args, _| {
        let dir = match as_ident(&args[0], "window-split")?.as_ref() {
            "vertical" | "v" => SplitDir::Vertical,
            "horizontal" | "h" => SplitDir::Horizontal,
            other => return Err(unknown_variant("window-split", other)),
        };
        apply(Action::WindowSplit(dir))?;
        Ok(unit())
    });
    b.be("window-close", 0, |_, _| {
        apply(Action::WindowClose)?;
        Ok(unit())
    });
    b.be("window-focus", 1, |args, _| {
        let dir = match as_ident(&args[0], "window-focus")?.as_ref() {
            "left" => FocusDir::Left,
            "right" => FocusDir::Right,
            "up" => FocusDir::Up,
            "down" => FocusDir::Down,
            other => return Err(unknown_variant("window-focus", other)),
        };
        apply(Action::WindowFocus(dir))?;
        Ok(unit())
    });
    b.be("window-focus-next", 0, |_, _| {
        apply(Action::WindowFocusNext)?;
        Ok(unit())
    });
}
