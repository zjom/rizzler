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
    b.be_doc(
        "insert-char",
        1,
        |args, _| {
            let s = as_str(&args[0], "insert-char")?;
            let c = s
                .chars()
                .next()
                .ok_or_else(|| str_mismatch("insert-char", "non-empty str"))?;
            apply(Action::InsertChar(c))?;
            Ok(unit())
        },
        "(insert-char STR)\n\nInserts the first character of STR at the cursor. The rest of STR is\nignored — for whole strings use (insert).\n\nErrors when STR is empty.\nSee also: (insert), (newline).",
    );
    b.be_doc(
        "insert",
        1,
        |args, _| {
            let s = as_str(&args[0], "insert")?;
            apply(Action::InsertMany(s))?;
            Ok(unit())
        },
        "(insert STR)\n\nInserts STR at the cursor in one tracked edit, advancing the cursor to\nits end. Embedded newlines are inserted literally.\nSee also: (insert-char), (newline).",
    );
    b.be_doc(
        "delete-char",
        0,
        |_, _| {
            apply(Action::DeleteChar)?;
            Ok(unit())
        },
        "(delete-char)\n\nDeletes the character under the cursor (vim `x`).\nSee also: (replace-backspace), (delete-char-at COL ROW).",
    );
    b.be_doc(
        "replace-backspace",
        0,
        |_, _| {
            apply(Action::ReplaceBackspace)?;
            Ok(unit())
        },
        "(replace-backspace)\n\nDeletes the character before the cursor — the backspace used while\nediting, as opposed to a motion-based delete.",
    );

    b.be_doc(
        "delete-char-at",
        2,
        |args, _| {
            let col = as_usize(&args[0], "delete-char-at")?;
            let row = as_usize(&args[1], "delete-char-at")?;
            apply(Action::DeleteCharAt(Position::new(col, row)))?;
            Ok(unit())
        },
        "(delete-char-at COL ROW)\n\nDeletes the character at absolute position (COL, ROW) in the focused\nbuffer, leaving the cursor where it is.\n\nCOL — int: 0-indexed column.\nROW — int: 0-indexed row.\nSee also: (delete-char).",
    );
    b.be_doc(
        "delete-selection",
        0,
        |_, _| {
            apply(Action::DeleteSelection)?;
            Ok(unit())
        },
        "(delete-selection)\n\nDeletes the active visual selection. A no-op when nothing is selected.\nSee also: (delete-inner OBJ), (delete-motion KIND).",
    );
    b.be_doc(
        "delete-line",
        0,
        |_, _| {
            let count = with_editor_mut(|st| st.pending_count_or_one());
            apply(Action::DeleteLine { count })?;
            Ok(unit())
        },
        "(delete-line)\n\nDeletes the current line linewise (vim `dd`), honoring the pending count\nprefix so `3 (delete-line)` removes three lines.\nSee also: (delete-motion KIND), (yank-line).",
    );
    b.be_doc(
        "indent-line",
        0,
        |_, _| {
            let count = with_editor_mut(|st| st.pending_count_or_one());
            apply(Action::ShiftLine {
                count,
                dedent: false,
            })?;
            Ok(unit())
        },
        "(indent-line)\n\nShifts the current line one shift width to the right (vim `>>`), honoring\nthe pending count prefix so `3 (indent-line)` shifts three lines. Blank\nlines are left untouched; the cursor lands on the first non-blank char.\nSee also: (dedent-line), (indent-selection).",
    );
    b.be_doc(
        "dedent-line",
        0,
        |_, _| {
            let count = with_editor_mut(|st| st.pending_count_or_one());
            apply(Action::ShiftLine {
                count,
                dedent: true,
            })?;
            Ok(unit())
        },
        "(dedent-line)\n\nShifts the current line one shift width to the left (vim `<<`), removing\nup to a shift width of leading whitespace and honoring the pending count\nprefix. The cursor lands on the first non-blank char.\nSee also: (indent-line), (dedent-selection).",
    );
    b.be_doc(
        "indent-selection",
        0,
        |_, _| {
            apply(Action::ShiftSelection { dedent: false })?;
            Ok(unit())
        },
        "(indent-selection)\n\nShifts every line the visual selection spans one shift width to the\nright (vim `>`), then returns to Normal mode. No-op outside a visual\nmode.\nSee also: (dedent-selection), (indent-line).",
    );
    b.be_doc(
        "dedent-selection",
        0,
        |_, _| {
            apply(Action::ShiftSelection { dedent: true })?;
            Ok(unit())
        },
        "(dedent-selection)\n\nShifts every line the visual selection spans one shift width to the left\n(vim `<`), then returns to Normal mode. No-op outside a visual mode.\nSee also: (indent-selection), (dedent-line).",
    );
    b.be_doc(
        "delete-motion",
        1,
        |args, _| {
            let sym = as_ident(&args[0], "delete-motion")?;
            let kind =
                MoveKind::from_str(&sym).map_err(|_| unknown_variant("delete-motion", &sym))?;
            let count = with_editor_mut(|st| st.pending_count_or_one());
            apply(Action::DeleteMotion { kind, count })?;
            Ok(unit())
        },
        "(delete-motion KIND)\n\nDeletes the text the motion KIND would cover from the cursor (vim\noperator+motion, e.g. `dw`), honoring the pending count prefix.\n\nKIND — move-kind: the motion to sweep, e.g. 'word-next, 'line-end.\n\nErrors when KIND is not a known motion.\nSee also: (move-cursor KIND), (yank-motion KIND).",
    );
    b.be_doc(
        "newline",
        0,
        |_, _| {
            apply(Action::InsertNewline)?;
            Ok(unit())
        },
        "(newline)\n\nInserts a line break at the cursor, splitting the current line. The new\nline copies the leading whitespace of the line being split (autoindent).\nSee also: (insert STR), (open-line-above).",
    );
    b.be_doc(
        "open-line-above",
        0,
        |_, _| {
            apply(Action::OpenLineAbove)?;
            Ok(unit())
        },
        "(open-line-above)\n\nOpens a blank line above the cursor's line and moves the cursor onto it,\ncopying that line's leading whitespace (autoindent). Drives vim `O`.\nSee also: (newline).",
    );
    b.be_doc(
        "undo",
        0,
        |_, _| {
            apply(Action::Undo)?;
            Ok(unit())
        },
        "(undo)\n\nUndoes the last tracked edit, honoring the pending count prefix.\nSee also: (redo).",
    );
    b.be_doc(
        "redo",
        0,
        |_, _| {
            apply(Action::Redo)?;
            Ok(unit())
        },
        "(redo)\n\nReapplies the last undone edit, honoring the pending count prefix.\nSee also: (undo).",
    );
    b.be_doc(
        "goto-last-edit",
        0,
        |_, _| {
            let count = with_editor_mut(|st| st.pending_count_or_one());
            apply(Action::GotoLastEdit { count })?;
            Ok(unit())
        },
        "(goto-last-edit)\n\nMoves the cursor to the site of the most recent edit. With the pending\ncount prefix, steps back that many entries in the edit history.",
    );
}
