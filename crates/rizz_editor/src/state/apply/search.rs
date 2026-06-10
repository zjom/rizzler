//! Search arms of the funnel: submit/cancel of the live `/` minibuffer
//! plus `n`/`N` repeats.

use rizz_search::SearchDir;
use tracing::debug;

use crate::state::State;

pub(super) fn submit(st: &mut State) {
    let pattern = st.bufs.minibuffer().text();
    debug!(pattern, "Action::SearchSubmit");
    if pattern.is_empty() {
        // Vim's `/<enter>` semantic: repeat last search forward from
        // wherever live search left the cursor.
        st.search.take_origin();
        st.exit_minibuffer();
        if st.search.last_pattern().is_some() {
            rizz_search::repeat_search(st, SearchDir::Forward);
        }
    } else {
        // Live search already placed cursor + overlays. Just record the
        // pattern in `/` and drop origin so cancel can't fire later.
        // Center the viewport on the match (vim's `nzz`, applied to
        // submit as well as n/N).
        st.search.take_origin();
        st.registers.record_search(&*pattern);
        st.exit_minibuffer();
        let target_id = st.surface.windows.focused_buf();
        if let Some(b) = st.bufs.get_mut(target_id) {
            b.move_cursor(rizz_text::MoveKind::Center);
        }
    }
}

pub(super) fn cancel(st: &mut State) {
    debug!("Action::SearchCancel");
    rizz_search::cancel_live_search(st);
    st.exit_minibuffer();
}

pub(super) fn repeat(st: &mut State, dir: SearchDir) {
    debug!(?dir, "repeat search");
    rizz_search::repeat_search(st, dir);
}
