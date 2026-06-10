//! Cursor motion builtins: named moves, relative deltas, line jumps.

use std::str::FromStr;

use rizz_actions::Action;
use rizz_core::Position;
use rizz_text::MoveKind;

use super::super::helpers::{Builtins, apply, as_ident, as_int, unit, unknown_variant};
use super::super::with_editor_mut;

pub(super) fn register(b: &mut Builtins) {
    b.be_doc(
        "move-cursor",
        1,
        |args, _| {
            let sym = as_ident(&args[0], "move-cursor")?;
            let mk = MoveKind::from_str(&sym).map_err(|_| unknown_variant("move-cursor", &sym))?;
            let count = with_editor_mut(|st| st.pending_count_or_one());
            apply(Action::MoveCursor { kind: mk, count })?;
            Ok(unit())
        },
        "(move-cursor KIND)\n\nMoves the cursor by the named motion KIND, honoring the pending count\nprefix so `5 (move-cursor 'down)` moves five rows.\n\nKIND — move-kind: the motion, e.g. 'left 'right 'up 'down 'word-next\n       'word-prev 'line-start 'line-end 'buffer-start 'buffer-end.\n\nErrors when KIND is not a known motion.\nSee also: (move-cursor-rel DX DY), (line N).",
    );
    b.be_doc(
        "move-cursor-rel",
        2,
        |args, _| {
            let dx = as_int(&args[0], "move-cursor-rel")?;
            let dy = as_int(&args[1], "move-cursor-rel")?;
            let mk = MoveKind::Relative(Position::new(dx as i16, dy as i16));
            let count = with_editor_mut(|st| st.pending_count_or_one());
            apply(Action::MoveCursor { kind: mk, count })?;
            Ok(unit())
        },
        "(move-cursor-rel DX DY)\n\nMoves the cursor by a relative delta: DX columns and DY rows. Negative\nvalues move left / up. Honors the pending count prefix.\n\nDX — int: column delta.\nDY — int: row delta.\nSee also: (move-cursor KIND).",
    );
    b.be_doc(
        "line",
        1,
        |args, _| {
            let n = as_int(&args[0], "line")?;
            let mk = MoveKind::LineNum(n.max(0) as usize);
            apply(Action::MoveCursor { kind: mk, count: 1 })?;
            Ok(unit())
        },
        "(line N)\n\nJumps the cursor to line N (0-indexed; negatives clamp to 0). This is\nwhat a bare `:42` in the command line resolves to.\n\nN — int: target line number.\nSee also: (move-cursor 'buffer-end), (cursor-line).",
    );
}
