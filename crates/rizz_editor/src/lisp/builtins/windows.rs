//! Window split/close/focus builtins.

use rizz_actions::Action;
use rizz_core::{FocusDir, SplitDir};

use super::super::helpers::{Builtins, apply, as_ident, unit, unknown_variant};

pub(super) fn register(b: &mut Builtins) {
    b.be_doc(
        "window-split",
        1,
        |args, _| {
            let dir = match as_ident(&args[0], "window-split")?.as_ref() {
                "vertical" | "v" => SplitDir::Vertical,
                "horizontal" | "h" => SplitDir::Horizontal,
                other => return Err(unknown_variant("window-split", other)),
            };
            apply(Action::WindowSplit(dir))?;
            Ok(unit())
        },
        "(window-split DIR)\n\nSplits the focused window in two, both halves viewing the current\nbuffer.\n\nDIR — ident: 'vertical (or 'v) stacks top/bottom; 'horizontal (or 'h)\n      places side by side.\n\nErrors when DIR is none of those idents.\nSee also: (window-close), (window-focus DIR).",
    );
    b.be_doc(
        "window-close",
        0,
        |_, _| {
            apply(Action::WindowClose)?;
            Ok(unit())
        },
        "(window-close)\n\nCloses the focused window, giving its space back to its sibling. A\nno-op when only one window is open.\nSee also: (window-split DIR).",
    );
    b.be_doc(
        "window-focus",
        1,
        |args, _| {
            let dir = match as_ident(&args[0], "window-focus")?.as_ref() {
                "left" => FocusDir::Left,
                "right" => FocusDir::Right,
                "up" => FocusDir::Up,
                "down" => FocusDir::Down,
                other => return Err(unknown_variant("window-focus", other)),
            };
            apply(Action::WindowFocus(dir))?;
            Ok(unit())
        },
        "(window-focus DIR)\n\nMoves focus to the window adjacent in direction DIR.\n\nDIR — ident: 'left 'right 'up 'down.\n\nErrors when DIR is none of those idents.\nSee also: (window-focus-next), (window-split DIR).",
    );
    b.be_doc(
        "window-focus-next",
        0,
        |_, _| {
            apply(Action::WindowFocusNext)?;
            Ok(unit())
        },
        "(window-focus-next)\n\nCycles focus to the next window in the layout, wrapping around. A\ndirection-agnostic alternative to (window-focus DIR).",
    );
}
