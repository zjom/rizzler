//! Text-mutation builtins: insert/delete/undo/redo and friends.

use std::str::FromStr;

use rizz_actions::Action;
use rizz_core::Position;
use rizz_text::MoveKind;

use super::super::helpers::{
    Builtins, apply, as_ident, as_str, as_usize, str_mismatch, unit, unknown_variant,
};
use super::super::with_editor_mut;

pub(super) fn register(b: &mut Builtins) {
    b.be("insert-char", 1, |args, _| {
        let s = as_str(&args[0], "insert-char")?;
        let c = s
            .chars()
            .next()
            .ok_or_else(|| str_mismatch("insert-char", "non-empty str"))?;
        apply(Action::InsertChar(c))?;
        Ok(unit())
    });
    b.be("insert", 1, |args, _| {
        let s = as_str(&args[0], "insert")?;
        apply(Action::InsertMany(s))?;
        Ok(unit())
    });
    b.be("delete-char", 0, |_, _| {
        apply(Action::DeleteChar)?;
        Ok(unit())
    });
    b.be("replace-backspace", 0, |_, _| {
        apply(Action::ReplaceBackspace)?;
        Ok(unit())
    });

    b.be("delete-char-at", 2, |args, _| {
        let col = as_usize(&args[0], "delete-char-at")?;
        let row = as_usize(&args[1], "delete-char-at")?;
        apply(Action::DeleteCharAt(Position::new(col, row)))?;
        Ok(unit())
    });
    b.be("delete-selection", 0, |_, _| {
        apply(Action::DeleteSelection)?;
        Ok(unit())
    });
    b.be("delete-line", 0, |_, _| {
        let count = with_editor_mut(|st| st.pending_count_or_one());
        apply(Action::DeleteLine { count })?;
        Ok(unit())
    });
    b.be("delete-motion", 1, |args, _| {
        let sym = as_ident(&args[0], "delete-motion")?;
        let kind = MoveKind::from_str(&sym).map_err(|_| unknown_variant("delete-motion", &sym))?;
        let count = with_editor_mut(|st| st.pending_count_or_one());
        apply(Action::DeleteMotion { kind, count })?;
        Ok(unit())
    });
    b.be("newline", 0, |_, _| {
        apply(Action::InsertNewline)?;
        Ok(unit())
    });
    b.be("undo", 0, |_, _| {
        apply(Action::Undo)?;
        Ok(unit())
    });
    b.be("redo", 0, |_, _| {
        apply(Action::Redo)?;
        Ok(unit())
    });
    b.be("goto-last-edit", 0, |_, _| {
        let count = with_editor_mut(|st| st.pending_count_or_one());
        apply(Action::GotoLastEdit { count })?;
        Ok(unit())
    });
}
