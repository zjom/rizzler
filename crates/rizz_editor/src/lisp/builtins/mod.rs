//! Per-domain builtin registrations. Each submodule exposes a
//! `register(&mut Builtins)` function that pushes its native functions onto
//! the shared registry; [`register_all`] composes them. Aliases are resolved
//! after every primary entry is in, so the order here is purely cosmetic.

mod bufs;
mod fs;
mod grammar;
mod keymap;
mod lifecycle;
mod lsp;
mod minibuffer;
mod misc;
mod motion;
mod popups;
mod queries;
mod registers;
mod search;
mod styling;
mod text;
mod textprops;
mod widgets;
mod windows;
mod wrap;

use super::helpers::Builtins;

pub(super) fn register_all(b: &mut Builtins) {
    lifecycle::register(b);
    text::register(b);
    motion::register(b);
    bufs::register(b);
    windows::register(b);
    keymap::register(b);
    minibuffer::register(b);
    popups::register(b);
    queries::register(b);
    registers::register(b);
    search::register(b);
    wrap::register(b);
    styling::register(b);
    widgets::register(b);
    textprops::register(b);
    misc::register(b);
    fs::register(b);
    grammar::register(b);
    lsp::register(b);
}
