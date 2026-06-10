//! Text-editing arms of the funnel: character edits, Replace-mode and
//! speculative-insert sessions, undo/redo, and cursor motion. Free
//! functions over `State`, called from the dispatch table in
//! [`super::State::apply`]'s `apply_one`.

use rizz_core::Position;
use rizz_text::{MoveKind, TextObject};
use tracing::{debug, trace};

use crate::state::State;

pub(super) fn insert_char(st: &mut State, c: char) {
    let f = st.focused_buf_id();
    st.bufs[f].insert_char(c);
}

pub(super) fn replace_char(st: &mut State, c: char) {
    let count = st.input.count_prefix.or_one();
    let f = st.focused_buf_id();
    debug!(buf = ?f, ch = %c, count, "Action::ReplaceChar");
    st.bufs[f].replace_char_n(c, count);
}

pub(super) fn overwrite_char(st: &mut State, c: char) {
    let f = st.focused_buf_id();
    trace!(buf = ?f, ch = %c, "Action::OverwriteChar");
    st.bufs[f].overwrite_char(c);
}

pub(super) fn replace_backspace(st: &mut State) {
    let f = st.focused_buf_id();
    trace!(buf = ?f, "Action::ReplaceBackspace");
    st.bufs[f].replace_backspace();
}

pub(super) fn speculative_insert_char(st: &mut State, c: char) {
    let f = st.focused_buf_id();
    st.bufs[f].insert_speculative_char(c);
}

pub(super) fn commit_speculation(st: &mut State) {
    let f = st.focused_buf_id();
    st.bufs[f].commit_speculation();
}

pub(super) fn rollback_speculation(st: &mut State) {
    let f = st.focused_buf_id();
    st.bufs[f].rollback_speculation();
}

pub(super) fn insert_many(st: &mut State, s: &str) {
    let f = st.focused_buf_id();
    debug!(buf = ?f, len = s.len(), "Action::InsertMany");
    st.bufs[f].insert_many(s);
}

pub(super) fn delete_char(st: &mut State) {
    let f = st.focused_buf_id();
    st.bufs[f].delete_char();
}

pub(super) fn delete_char_at(st: &mut State, pos: Position<usize>) {
    let f = st.focused_buf_id();
    st.bufs[f].delete_char_at(pos);
}

pub(super) fn undo(st: &mut State) {
    let f = st.focused_buf_id();
    debug!(buf = ?f, "Action::Undo");
    st.bufs[f].undo();
    st.bufs[f].move_cursor(MoveKind::Center);
}

pub(super) fn redo(st: &mut State) {
    let f = st.focused_buf_id();
    debug!(buf = ?f, "Action::Redo");
    st.bufs[f].redo();
    st.bufs[f].move_cursor(MoveKind::Center);
}

pub(super) fn goto_last_edit(st: &mut State, count: u32) {
    let f = st.focused_buf_id();
    debug!(buf = ?f, count, "Action::GotoLastEdit");
    st.bufs[f].goto_last_edit(count);
    st.bufs[f].move_cursor(MoveKind::Center);
}

pub(super) fn move_cursor(st: &mut State, kind: MoveKind, count: u32) {
    let f = st.focused_buf_id();
    trace!(buf = ?f, ?kind, count, "Action::MoveCursor");
    st.bufs[f].move_cursor_n(kind, count);
}

pub(super) fn select_text_object(st: &mut State, object: TextObject, around: bool, count: u32) {
    let f = st.focused_buf_id();
    debug!(buf = ?f, ?object, around, count, "Action::SelectTextObject");
    if let Some((lo, hi, _)) = st.bufs[f].text_object_range(object, around, count) {
        st.bufs[f].select_char_range(lo, hi);
    }
}
