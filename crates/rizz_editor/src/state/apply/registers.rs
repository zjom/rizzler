//! Register-routing arms of the funnel: yank/delete capture, paste, and
//! the `"a` register prefix.
//!
//! Every delete/yank arm funnels through [`record_deleted`] /
//! [`record_yanked`] so the pending-register protocol (take on success;
//! a failed yank clears the prefix, a failed delete leaves it staged)
//! lives in exactly one place.

use rizz_core::EditingMode;
use rizz_registers::{RegisterEntry, RegisterKind};
use rizz_text::{MoveKind, TextObject};
use std::rc::Rc;
use tracing::{debug, trace};

use crate::state::State;

/// Shared tail of every delete arm: when the delete succeeded and captured
/// text, route it to the pending (or unnamed) register.
fn record_deleted(st: &mut State, deleted: bool, yanked: Option<(String, RegisterKind)>) {
    if deleted && let Some((text, kind)) = yanked {
        let name = st.pending_register.take();
        st.registers.record_delete(text, kind, name);
    }
}

/// Shared tail of every yank arm: record into the pending (or unnamed)
/// register, or clear the pending prefix when nothing was yanked.
fn record_yanked(st: &mut State, yanked: Option<(String, RegisterKind)>) {
    match yanked {
        Some((text, kind)) => {
            let name = st.pending_register.take();
            st.registers.record_yank(text, kind, name);
        }
        None => st.pending_register = None,
    }
}

pub(super) fn delete_selection(st: &mut State) {
    let f = st.focused_buf_id();
    let yanked = st.bufs[f].yank_selection();
    let deleted = st.bufs[f].delete_selection();
    record_deleted(st, deleted, yanked);
}

pub(super) fn delete_line(st: &mut State, count: u32) {
    let f = st.focused_buf_id();
    debug!(buf = ?f, count, "Action::DeleteLine");
    let yanked = st.bufs[f].yank_line(count);
    let deleted = st.bufs[f].delete_line(count);
    record_deleted(st, deleted, yanked);
}

pub(super) fn delete_motion(st: &mut State, kind: MoveKind, count: u32) {
    let f = st.focused_buf_id();
    debug!(buf = ?f, ?kind, count, "Action::DeleteMotion");
    let yanked = st.bufs[f].yank_motion(kind, count);
    let deleted = st.bufs[f].delete_motion(kind, count);
    record_deleted(st, deleted, yanked);
}

pub(super) fn yank_motion(st: &mut State, kind: MoveKind, count: u32) {
    let f = st.focused_buf_id();
    debug!(buf = ?f, ?kind, count, "Action::YankMotion");
    let yanked = st.bufs[f].yank_motion(kind, count);
    record_yanked(st, yanked);
}

pub(super) fn yank_line(st: &mut State, count: u32) {
    let f = st.focused_buf_id();
    debug!(buf = ?f, count, "Action::YankLine");
    let yanked = st.bufs[f].yank_line(count);
    record_yanked(st, yanked);
}

pub(super) fn yank_selection(st: &mut State) {
    let f = st.focused_buf_id();
    debug!(buf = ?f, "Action::YankSelection");
    let yanked = st.bufs[f].yank_selection();
    record_yanked(st, yanked);
    st.bufs[f].set_mode(EditingMode::Normal);
}

pub(super) fn paste(st: &mut State, before: bool, count: u32) {
    let name = st.pending_register.take().unwrap_or('"');
    let Some(entry) = st.registers.read(name).cloned() else {
        trace!(?name, "Action::Paste: empty register");
        return;
    };
    let f = st.focused_buf_id();
    debug!(buf = ?f, ?name, before, count, "Action::Paste");
    // Vim's `Np` inserts N copies of the register payload in one shot,
    // not N successive pastes.
    let n = count.max(1) as usize;
    let entry = if n > 1 {
        let mut joined = String::with_capacity(entry.text.len() * n);
        for _ in 0..n {
            joined.push_str(&entry.text);
        }
        RegisterEntry::new(joined, entry.kind)
    } else {
        entry
    };
    st.bufs[f].paste(&entry, before);
}

pub(super) fn register_set(st: &mut State, name: char, text: Rc<str>, kind: RegisterKind) {
    debug!(name = ?name, kind = ?kind, "Action::RegisterSet");
    st.registers.write(name, RegisterEntry::new(text, kind));
}

pub(super) fn delete_text_object(st: &mut State, object: TextObject, around: bool, count: u32) {
    let f = st.focused_buf_id();
    debug!(buf = ?f, ?object, around, count, "Action::DeleteTextObject");
    if let Some((lo, hi, kind)) = st.bufs[f].text_object_range(object, around, count) {
        let text = st.bufs[f].rope().slice(lo..hi).to_string();
        let deleted = st.bufs[f].delete_range(lo, hi);
        record_deleted(st, deleted, Some((text, kind)));
    } else {
        st.pending_register = None;
    }
}

pub(super) fn yank_text_object(st: &mut State, object: TextObject, around: bool, count: u32) {
    let f = st.focused_buf_id();
    debug!(buf = ?f, ?object, around, count, "Action::YankTextObject");
    if let Some((lo, hi, kind)) = st.bufs[f].text_object_range(object, around, count) {
        let text = st.bufs[f].rope().slice(lo..hi).to_string();
        record_yanked(st, Some((text, kind)));
    } else {
        st.pending_register = None;
    }
}
