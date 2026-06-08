//! Per-domain builtin registrations. Each submodule exposes a
//! `register(&mut Builtins)` function that pushes its native functions onto
//! the shared registry; [`register_all`] composes them in a fixed order.
//!
//! The order here only matters to the `alias` resolution pass (aliases must
//! be able to look up their target by the time [`Builtins::build`] runs), and
//! since aliases are collected and resolved at the end, the ordering inside
//! `register_all` is purely for readability.

mod bufs;
mod fs;
mod grammar;
mod keymap;
mod lifecycle;
mod minibuffer;
mod misc;
mod motion;
mod popups;
mod queries;
mod registers;
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
    wrap::register(b);
    styling::register(b);
    widgets::register(b);
    textprops::register(b);
    misc::register(b);
    fs::register(b);
    grammar::register(b);
}
