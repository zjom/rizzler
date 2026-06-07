use std::str::FromStr;

use rizz_actions::Action;
use rizz_core::Position;
use rizz_text::MoveKind;

use super::super::helpers::{Builtins, apply, as_ident, as_int, unit, unknown_variant};
use super::super::with_editor_mut;

pub(super) fn register(b: &mut Builtins) {
    b.be("move-cursor", 1, |args, _| {
        let sym = as_ident(&args[0], "move-cursor")?;
        let mk = MoveKind::from_str(&sym).map_err(|_| unknown_variant("move-cursor", &sym))?;
        let count = with_editor_mut(|st| st.pending_count_or_one());
        apply(Action::MoveCursor { kind: mk, count })?;
        Ok(unit())
    });
    b.be("move-cursor-rel", 2, |args, _| {
        let dx = as_int(&args[0], "move-cursor-rel")?;
        let dy = as_int(&args[1], "move-cursor-rel")?;
        let mk = MoveKind::Relative(Position::new(dx as i16, dy as i16));
        let count = with_editor_mut(|st| st.pending_count_or_one());
        apply(Action::MoveCursor { kind: mk, count })?;
        Ok(unit())
    });
    b.be("line", 1, |args, _| {
        let n = as_int(&args[0], "line")?;
        let mk = MoveKind::LineNum(n.max(0) as usize);
        apply(Action::MoveCursor { kind: mk, count: 1 })?;
        Ok(unit())
    });
}
