//! Vim-style `/` search.
//!
//! Given a regex pattern and a target buffer, [`run`] finds every match,
//! drops one overlay per match using the `search-match` face, and jumps the
//! cursor to the destination match (the first match at-or-after the supplied
//! origin for forward search, with wrap). The pattern and direction of the
//! last successful run are kept on the [`Search`] so `n` / `N` can repeat
//! without re-prompting.
//!
//! Live (`incsearch`) flow: when the user enters Search mode the caller
//! stashes the origin cursor on [`Search::set_origin`] and replays [`run`]
//! after every minibuffer keystroke. [`clear_overlays`] drops the
//! highlights when the pattern becomes empty or the user cancels — without
//! needing a valid regex.

use std::rc::Rc;

use rizz::runtime::Value;
use rizz_core::Position;
use rizz_text::props::{OverlayId, PropEntry};
use rizz_text::{Buffer, BufferId};

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
    pub cursor: Position<usize>,
}

/// Per-`State` search bookkeeping: last submitted pattern + direction (for
/// `n`/`N`), the overlay ids painted by the previous run, and the live-mode
/// origin used while the minibuffer is open.
#[derive(Default)]
pub struct Search {
    last_pattern: Option<Rc<str>>,
    last_dir: SearchDir,
    overlays: Vec<(BufferId, OverlayId)>,
    origin: Option<SearchOrigin>,
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
        self.origin = Some(origin);
    }

    pub fn take_origin(&mut self) -> Option<SearchOrigin> {
        self.origin.take()
    }
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
    // For now we materialize once per search — fine for typical files.
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
                .find(|(s, _)| if inclusive { *s >= from_char } else { *s > from_char })
                .map(|m| m.0)
                .or_else(|| matches.first().map(|m| m.0)),
            SearchDir::Backward => matches
                .iter()
                .rev()
                .find(|(s, _)| if inclusive { *s <= from_char } else { *s < from_char })
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
