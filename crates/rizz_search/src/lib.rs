//! Vim-style `/` search.
//!
//! Two layers:
//!
//! - [`Search`] + [`run`] / [`clear_overlays`] are the pure mechanics — given a
//!   pattern and a target `Buffer`, paint one overlay per match and jump the
//!   cursor to the destination. The pattern + direction of the last successful
//!   run are kept on [`Search`] so `n` / `N` can repeat without re-prompting.
//! - [`SearchHost`] is the trait an editor (e.g. `rizz_editor::State`)
//!   implements to expose the bits the high-level helpers
//!   ([`run_search_from`], [`repeat_search`], [`refresh_live_search`],
//!   [`clear_live_overlays`], [`cancel_live_search`], [`restore_origin`])
//!   need: the [`Search`] field, the focused buffer id, the buffer storage,
//!   the minibuffer text, and a notification sink.
//!
//! Live (`incsearch`) flow: when the user enters Search mode the host stashes
//! the origin cursor on [`Search::set_origin`] and the editor replays
//! [`refresh_live_search`] after every minibuffer keystroke. Empty pattern (or
//! a partial regex like `[`) clears highlights and rewinds the cursor to
//! origin so the user sees the original buffer back. `<esc>` invokes
//! [`cancel_live_search`].

use std::rc::Rc;

use rizz::runtime::Value;
use rizz_core::{FilePos, Position};
use rizz_text::props::{OverlayId, PropEntry};
use rizz_text::{Buffer, BufferId, MoveKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SearchDir {
    #[default]
    Forward,
    Backward,
}

impl SearchDir {
    pub fn reverse(self) -> Self {
        match self {
            SearchDir::Forward => SearchDir::Backward,
            SearchDir::Backward => SearchDir::Forward,
        }
    }
}

/// The cursor position the user pressed `/` at, captured so live search can
/// restart matching from there every keystroke (and so cancel restores it).
/// The viewport snaps back automatically — `MoveKind::Absolute` re-clamps
/// the scroll top around the restored cursor.
#[derive(Clone, Copy, Debug)]
pub struct SearchOrigin {
    pub buf: BufferId,
    pub cursor: FilePos,
}

/// Per-host search bookkeeping: last submitted pattern + direction (for
/// `n`/`N`), the overlay ids painted by the previous run, and the live-mode
/// origin used while the minibuffer is open.
#[derive(Default)]
pub struct Search {
    last_pattern: Option<Rc<str>>,
    last_dir: SearchDir,
    overlays: Vec<(BufferId, OverlayId)>,
    origin: Option<SearchOrigin>,
    /// Buffer the current/last search targets. Set alongside `origin` on `/`
    /// entry and survives `take_origin` so `n`/`N` keep targeting the same
    /// buffer (e.g. a popup) after the minibuffer closes. Hosts consult this
    /// from their `focused_buf_id` so that opening `/` inside a popup runs
    /// search against the popup's buffer rather than the editor window
    /// underneath.
    target_buf: Option<BufferId>,
}

impl Search {
    pub fn last_pattern(&self) -> Option<&Rc<str>> {
        self.last_pattern.as_ref()
    }

    pub fn last_dir(&self) -> SearchDir {
        self.last_dir
    }

    pub fn origin(&self) -> Option<SearchOrigin> {
        self.origin
    }

    pub fn set_origin(&mut self, origin: SearchOrigin) {
        self.target_buf = Some(origin.buf);
        self.origin = Some(origin);
    }

    pub fn take_origin(&mut self) -> Option<SearchOrigin> {
        self.origin.take()
    }

    pub fn target_buf(&self) -> Option<BufferId> {
        self.target_buf
    }

    pub fn clear_target(&mut self) {
        self.target_buf = None;
    }
}

/// Editor surface the high-level helpers need.
///
/// `search_and_buf_mut` is the combined accessor that lets `run` get
/// `&mut Search` and `&mut Buffer` at the same time — splitting them across
/// two trait calls would force the impl into RefCell territory or runtime
/// borrow checks.
pub trait SearchHost {
    fn search(&self) -> &Search;
    fn search_mut(&mut self) -> &mut Search;
    fn focused_buf_id(&self) -> BufferId;
    fn buf(&self, id: BufferId) -> Option<&Buffer>;
    fn buf_mut(&mut self, id: BufferId) -> Option<&mut Buffer>;
    fn search_and_buf_mut(&mut self, id: BufferId) -> Option<(&mut Search, &mut Buffer)>;
    fn minibuffer_text(&self) -> String;
    fn notify(&mut self, msg: &str);
}

/// Drop every overlay this `Search` previously painted. Used by cancel and
/// by the empty-pattern path of live search. The caller-supplied `clear_other`
/// hook removes overlays in buffers other than `prefer`, so the function
/// stays loan-free.
pub fn clear_overlays<F>(search: &mut Search, prefer: Option<&mut Buffer>, mut clear_other: F)
where
    F: FnMut(BufferId, OverlayId),
{
    let prefer_id = search
        .overlays
        .first()
        .map(|(b, _)| *b)
        .filter(|_| prefer.is_some());
    if let (Some(buf), Some(target_id)) = (prefer, prefer_id) {
        for (buf_id, ov) in search.overlays.drain(..) {
            if buf_id == target_id {
                buf.props_mut().delete_overlay(ov);
            } else {
                clear_other(buf_id, ov);
            }
        }
    } else {
        for (buf_id, ov) in search.overlays.drain(..) {
            clear_other(buf_id, ov);
        }
    }
}

/// Run `pattern` against `target_buf` in `dir`, painting one overlay per
/// match and jumping the cursor to the destination match.
///
/// `from_char` is the rope char index that anchors the "next match" choice:
/// forward returns the first match at-or-after `from_char` when `inclusive`,
/// else the first match strictly after; backward is the symmetric case.
/// Wrap is unconditional — vim does the same and surfaces "search hit
/// BOTTOM/TOP" as a notice we elide for now.
#[allow(clippy::too_many_arguments)]
pub fn run<F>(
    search: &mut Search,
    target_buf: &mut Buffer,
    target_id: BufferId,
    pattern: &str,
    dir: SearchDir,
    from_char: usize,
    inclusive: bool,
    mut clear_other: F,
) -> Result<bool, regex::Error>
where
    F: FnMut(BufferId, OverlayId),
{
    let re = regex::Regex::new(pattern)?;

    for (buf_id, ov) in search.overlays.drain(..) {
        if buf_id == target_id {
            target_buf.props_mut().delete_overlay(ov);
        } else {
            clear_other(buf_id, ov);
        }
    }

    // TODO: streaming match over `rope.chunks()` for very large buffers.
    let rope = target_buf.rope().clone();
    let text = rope.to_string();

    let matches: Vec<(usize, usize)> = re
        .find_iter(&text)
        .map(|m| {
            let s = rope.byte_to_char(m.start());
            let e = rope.byte_to_char(m.end());
            (s, e)
        })
        .collect();

    let face: Rc<Value> = Rc::new(Value::Ident("search-match".into()));
    for (s, e) in &matches {
        let sr = rope.char_to_line(*s);
        let sc = *s - rope.line_to_char(sr);
        let er = rope.char_to_line(*e);
        let ec = *e - rope.line_to_char(er);
        let id = target_buf.props_mut().create_overlay(PropEntry {
            start: Position::new(sc, sr),
            end: Position::new(ec, er),
            face: Some(face.clone()),
            display: None,
            priority: 0,
            pad_to_width: false,
        });
        search.overlays.push((target_id, id));
    }

    if !matches.is_empty() {
        let dest = match dir {
            SearchDir::Forward => matches
                .iter()
                .find(|(s, _)| {
                    if inclusive {
                        *s >= from_char
                    } else {
                        *s > from_char
                    }
                })
                .map(|m| m.0)
                .or_else(|| matches.first().map(|m| m.0)),
            SearchDir::Backward => matches
                .iter()
                .rev()
                .find(|(s, _)| {
                    if inclusive {
                        *s <= from_char
                    } else {
                        *s < from_char
                    }
                })
                .map(|m| m.0)
                .or_else(|| matches.last().map(|m| m.0)),
        };
        if let Some(cidx) = dest {
            target_buf.move_cursor_to_char(cidx);
        }
    }

    search.last_pattern = Some(pattern.into());
    search.last_dir = dir;
    Ok(!matches.is_empty())
}

/// Execute a single forward/backward search against the host's focused
/// buffer. `from_char` is the rope char index the match search anchors to;
/// `inclusive` decides whether a match starting exactly at `from_char`
/// counts. `notify_on_miss` toggles the "Pattern not found" toast — off for
/// the live-search path so every partial keystroke doesn't spam
/// notifications.
pub fn run_search_from<H: SearchHost + ?Sized>(
    host: &mut H,
    pattern: &str,
    dir: SearchDir,
    from_char: usize,
    inclusive: bool,
    notify_on_miss: bool,
) -> Result<bool, regex::Error> {
    let target_id = host.focused_buf_id();
    let mut other_clears: Vec<(BufferId, OverlayId)> = Vec::new();
    let r = {
        let Some((search, target)) = host.search_and_buf_mut(target_id) else {
            return Ok(false);
        };
        run(
            search,
            target,
            target_id,
            pattern,
            dir,
            from_char,
            inclusive,
            |bid, ov| other_clears.push((bid, ov)),
        )
    };
    for (bid, ov) in other_clears {
        if let Some(b) = host.buf_mut(bid) {
            b.props_mut().delete_overlay(ov);
        }
    }
    match &r {
        Ok(false) if notify_on_miss => {
            host.notify(&format!("Pattern not found: {pattern}"));
        }
        Err(e) if notify_on_miss => {
            host.notify(&format!("Invalid pattern: {e}"));
        }
        _ => {}
    }
    r
}

/// Re-run the last `/` pattern in `dir`. `n` always means forward, `N`
/// always means backward — independent of how the pattern was entered.
/// Notifies when no previous pattern has been stored. Centers the viewport
/// on the destination match (vim's `nzz` flow).
pub fn repeat_search<H: SearchHost + ?Sized>(host: &mut H, dir: SearchDir) {
    let Some(pattern) = host.search().last_pattern().cloned() else {
        host.notify("No previous search pattern");
        return;
    };
    let target_id = host.focused_buf_id();
    let cursor_char = match host.buf(target_id) {
        Some(buf) => {
            let p = buf.abs_pos();
            buf.rope().line_to_char(p.row) + p.col
        }
        None => return,
    };
    // Advance past the current match — vim's `n`/`N` semantic (exclusive).
    let found =
        run_search_from(host, pattern.as_ref(), dir, cursor_char, false, true).unwrap_or(false);
    if found && let Some(b) = host.buf_mut(target_id) {
        b.move_cursor(MoveKind::Center);
    }
}

/// Re-paint the live search against the current minibuffer pattern,
/// anchored at the origin captured on `/` entry. Empty pattern (or an
/// invalid partial regex like `[`) clears highlights and rewinds the
/// cursor to origin so the user sees the original buffer back.
pub fn refresh_live_search<H: SearchHost + ?Sized>(host: &mut H) {
    let Some(origin) = host.search().origin() else {
        return;
    };
    let pattern = host.minibuffer_text();
    // Always restart from origin: cursor sits there before the run, and
    // `run` jumps to the next match from there if the regex compiles +
    // matches at least once.
    restore_origin(host, origin);
    if pattern.is_empty() {
        clear_live_overlays(host);
        return;
    }
    let from_char = match host.buf(origin.buf) {
        Some(buf) => buf.rope().line_to_char(origin.cursor.row) + origin.cursor.col,
        None => return,
    };
    let r = run_search_from(host, &pattern, SearchDir::Forward, from_char, true, false);
    match r {
        Ok(true) => {
            // Center the viewport on each live match as the user types.
            // When the pattern stops matching, `restore_origin` above has
            // already snapped the cursor back, so there's no overshoot.
            if let Some(b) = host.buf_mut(origin.buf) {
                b.move_cursor(MoveKind::Center);
            }
        }
        Err(_) => {
            // Partial regex — drop stale paint without disturbing the cursor.
            clear_live_overlays(host);
        }
        _ => {}
    }
}

/// Drop every overlay the host's `Search` has painted across buffers.
pub fn clear_live_overlays<H: SearchHost + ?Sized>(host: &mut H) {
    let mut other_clears: Vec<(BufferId, OverlayId)> = Vec::new();
    let prefer_id = host
        .search()
        .origin()
        .map(|o| o.buf)
        .unwrap_or_else(|| host.focused_buf_id());
    match host.search_and_buf_mut(prefer_id) {
        Some((search, prefer)) => {
            clear_overlays(search, Some(prefer), |bid, ov| other_clears.push((bid, ov)));
        }
        None => {
            clear_overlays(host.search_mut(), None, |bid, ov| {
                other_clears.push((bid, ov))
            });
        }
    }
    for (bid, ov) in other_clears {
        if let Some(b) = host.buf_mut(bid) {
            b.props_mut().delete_overlay(ov);
        }
    }
}

/// `<esc>` while typing a `/` pattern: drop highlights and put the cursor +
/// scroll back where the user pressed `/`.
pub fn cancel_live_search<H: SearchHost + ?Sized>(host: &mut H) {
    if let Some(origin) = host.search_mut().take_origin() {
        restore_origin(host, origin);
    }
    clear_live_overlays(host);
}

/// Snap the cursor in `origin.buf` back to `origin.cursor`. The viewport
/// re-clamps via `MoveKind::Absolute` semantics on `move_cursor_to_char`.
pub fn restore_origin<H: SearchHost + ?Sized>(host: &mut H, origin: SearchOrigin) {
    if let Some(buf) = host.buf_mut(origin.buf) {
        let cidx = buf.rope().line_to_char(origin.cursor.row) + origin.cursor.col;
        buf.move_cursor_to_char(cidx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rizz_text::{Buffer, MoveKind};

    fn seeded_buffer(text: &str) -> Buffer {
        let mut b = Buffer::new();
        b.clear_with(text);
        b
    }

    fn cursor_char(buf: &Buffer) -> usize {
        let p = buf.abs_pos();
        buf.rope().line_to_char(p.row) + p.col
    }

    #[test]
    fn forward_inclusive_lands_on_first_match_at_origin() {
        let mut s = Search::default();
        let mut b = seeded_buffer("foo bar foo baz foo");
        let id = BufferId::default();
        let found = run(
            &mut s,
            &mut b,
            id,
            "foo",
            SearchDir::Forward,
            0,
            true,
            |_, _| {},
        )
        .unwrap();
        assert!(found);
        assert_eq!(b.props().overlays.len(), 3);
        let p = b.abs_pos();
        assert_eq!((p.row, p.col), (0, 0));
    }

    #[test]
    fn forward_exclusive_advances_past_current_match() {
        let mut s = Search::default();
        let mut b = seeded_buffer("foo bar foo baz foo");
        let id = BufferId::default();
        let from = cursor_char(&b);
        let _ = run(
            &mut s,
            &mut b,
            id,
            "foo",
            SearchDir::Forward,
            from,
            false,
            |_, _| {},
        )
        .unwrap();
        let p = b.abs_pos();
        assert_eq!((p.row, p.col), (0, 8));
    }

    #[test]
    fn backward_picks_previous_match() {
        let mut s = Search::default();
        let mut b = seeded_buffer("foo bar foo baz foo");
        let id = BufferId::default();
        b.move_cursor(MoveKind::Absolute(Position::new(12, 0)));
        let from = cursor_char(&b);
        let _ = run(
            &mut s,
            &mut b,
            id,
            "foo",
            SearchDir::Backward,
            from,
            false,
            |_, _| {},
        )
        .unwrap();
        let p = b.abs_pos();
        assert_eq!((p.row, p.col), (0, 8));
    }

    #[test]
    fn no_matches_leaves_cursor_alone() {
        let mut s = Search::default();
        let mut b = seeded_buffer("foo bar");
        let id = BufferId::default();
        b.move_cursor(MoveKind::Absolute(Position::new(3, 0)));
        let from = cursor_char(&b);
        let found = run(
            &mut s,
            &mut b,
            id,
            "zzz",
            SearchDir::Forward,
            from,
            true,
            |_, _| {},
        )
        .unwrap();
        assert!(!found);
        let p = b.abs_pos();
        assert_eq!((p.row, p.col), (0, 3));
    }

    #[test]
    fn invalid_pattern_is_an_error() {
        let mut s = Search::default();
        let mut b = seeded_buffer("foo");
        let id = BufferId::default();
        let err = run(
            &mut s,
            &mut b,
            id,
            "[",
            SearchDir::Forward,
            0,
            true,
            |_, _| {},
        );
        assert!(err.is_err());
    }
}
