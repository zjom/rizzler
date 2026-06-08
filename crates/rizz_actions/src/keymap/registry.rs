use std::collections::HashMap;
use std::rc::Rc;

use rizz_input::KeyEvent;
use tracing::{debug, trace};

use crate::action::Action;
use crate::keymap::default::default_keymaps;
use crate::keymap::trie::{Trie, TrieIter, WalkOutcome, walk, walk_flush};

/// One entry in the held chord prefix.
struct Pending {
    key: KeyEvent,
    /// Whether this key was speculated at `Descend` time — i.e. the keymap
    /// emitted a `SpeculativeInsertChar` so the user could see their typing
    /// before the chord resolved. On chord completion the buffer rolls back;
    /// on chord abort the speculation is committed and not re-flushed.
    speculated: bool,
}

pub struct KeymapRegistry {
    children: HashMap<Rc<str>, Rc<Trie>>,
    cur: Option<Rc<Trie>>,
    prev_mode: Option<Rc<str>>,
    /// The held chord prefix. We keep each original `KeyEvent` (so we can
    /// replay it on abort) plus whether the buffer already saw a
    /// speculative insert for it.
    pending: Vec<Pending>,
}

impl KeymapRegistry {
    pub fn new() -> Self {
        Self {
            children: default_keymaps(),
            cur: None,
            prev_mode: None,
            pending: Vec::new(),
        }
    }

    /// True when no key sequence is currently in flight. Callers use this to
    /// decide whether an incoming digit can be claimed as a count prefix or
    /// whether it might continue an existing binding (e.g. `g3`).
    pub fn is_idle(&self) -> bool {
        self.cur.is_none()
    }

    /// Resolve `key` against the supplied layered modes. `modes` is ordered
    /// most-specific first — the first mode whose trie produces an
    /// `Action` or `Descend` wins.
    ///
    /// While a chord prefix descends, any key whose mode would otherwise
    /// have inserted it as text (via `on_char`) is speculated: the keymap
    /// emits a `SpeculativeInsertChar` so the user sees the keystroke
    /// immediately. When the chord completes (`Action`), the buffer's
    /// speculation is rolled back before the chord's action runs. When the
    /// chord aborts (timeout, mode change, non-matching continuation), the
    /// speculation is committed and any non-speculated held keys are
    /// flushed as standalone resolutions, then the current key is resolved
    /// fresh from each mode's root.
    pub fn resolve(
        &mut self,
        modes: &[Rc<str>],
        key: KeyEvent,
        timedout: bool,
    ) -> Option<Vec<Rc<Action>>> {
        let continuing = !timedout
            && self
                .prev_mode
                .as_ref()
                .is_some_and(|pm| modes.iter().any(|m| m == pm));

        if continuing && let Some(cur) = self.cur.take() {
            match walk(&cur, key) {
                WalkOutcome::Action(a) => {
                    trace!(?key, prev_mode = ?self.prev_mode, "keymap continued -> action");
                    let needs_rollback = self.pending.iter().any(|p| p.speculated);
                    self.prev_mode = None;
                    self.pending.clear();
                    let mut actions = Vec::with_capacity(2);
                    if needs_rollback {
                        actions.push(Rc::new(Action::RollbackSpeculation));
                    }
                    actions.push(a);
                    return Some(actions);
                }
                WalkOutcome::Descend(n) => {
                    trace!(?key, prev_mode = ?self.prev_mode, "keymap continued -> descend");
                    let spec = self
                        .prev_mode
                        .clone()
                        .and_then(|m| self.speculate_action(&m, key));
                    self.cur = Some(n);
                    self.pending.push(Pending {
                        key,
                        speculated: spec.is_some(),
                    });
                    return spec.map(|a| vec![a]);
                }
                WalkOutcome::Miss => {
                    debug!(?key, prev_mode = ?self.prev_mode, "keymap continuation missed -> flush + restart");
                    // Fall through: commit any in-flight speculation, flush
                    // the non-speculated portion of the held prefix, then
                    // give the user's key a fresh top-level resolution.
                }
            }
        }
        let flush_modes = self.prev_mode.take().map(|m| vec![m]);
        self.cur = None;
        let mut actions = self.flush_pending(flush_modes.as_deref().unwrap_or(modes));

        for mode in modes {
            let Some(root) = self.children.get(mode).cloned() else {
                continue;
            };
            match walk(&root, key) {
                WalkOutcome::Action(a) => {
                    trace!(?key, %mode, "keymap resolved -> action");
                    actions.push(a);
                    return Some(actions);
                }
                WalkOutcome::Descend(n) => {
                    trace!(?key, %mode, "keymap resolved -> descend");
                    let spec = self.speculate_action(mode, key);
                    self.cur = Some(n);
                    self.prev_mode = Some(mode.clone());
                    self.pending.push(Pending {
                        key,
                        speculated: spec.is_some(),
                    });
                    if let Some(a) = spec {
                        actions.push(a);
                    }
                    return if actions.is_empty() {
                        None
                    } else {
                        Some(actions)
                    };
                }
                WalkOutcome::Miss => {}
            }
        }
        trace!(?key, ?modes, "keymap miss across all modes");
        if actions.is_empty() {
            None
        } else {
            Some(actions)
        }
    }

    /// If the descending key would have inserted as text in `mode` (via
    /// `walk_flush` resolving to `Action::InsertChar`), return the
    /// equivalent `SpeculativeInsertChar` so the buffer can show the user's
    /// keystroke immediately. Only `InsertChar` is speculation-eligible:
    /// other actions can't be cleanly rolled back via `RollbackSpeculation`.
    fn speculate_action(&self, mode: &Rc<str>, key: KeyEvent) -> Option<Rc<Action>> {
        let root = self.children.get(mode)?;
        let action = walk_flush(root.as_ref(), key)?;
        match action.as_ref() {
            Action::InsertChar(c) => Some(Rc::new(Action::SpeculativeInsertChar(*c))),
            _ => None,
        }
    }

    /// Build the action sequence emitted when a chord is abandoned.
    /// If anything was speculated, prepend a `CommitSpeculation` so the
    /// staged text lands in the undo history. Then replay each
    /// non-speculated held key via `walk_flush` (speculated keys are
    /// already in the buffer — committing them suffices).
    fn flush_pending(&mut self, modes: &[Rc<str>]) -> Vec<Rc<Action>> {
        if self.pending.is_empty() {
            return Vec::new();
        }
        let held = std::mem::take(&mut self.pending);
        let mut actions = Vec::new();
        if held.iter().any(|p| p.speculated) {
            actions.push(Rc::new(Action::CommitSpeculation));
        }
        for p in held {
            if p.speculated {
                continue;
            }
            for mode in modes {
                if let Some(root) = self.children.get(mode)
                    && let Some(a) = walk_flush(root.as_ref(), p.key)
                {
                    actions.push(a);
                    break;
                }
            }
        }
        actions
    }

    /// Bind a key sequence in `mode` to `action`.
    pub fn set(&mut self, mode: Rc<str>, keys: &[KeyEvent], action: Rc<Action>) {
        self.cur = None;
        self.pending.clear();
        let root = self
            .children
            .entry(mode)
            .or_insert_with(|| Rc::new(Trie::empty()));
        Trie::insert_path(root, keys, action);
    }

    /// Remove the binding at `keys` in `mode`, returning the removed action if
    /// it was a leaf. Drops the mode entirely if its root becomes empty.
    pub fn remove(&mut self, mode: Rc<str>, keys: &[KeyEvent]) -> Option<Rc<Action>> {
        self.cur = None;
        self.pending.clear();
        let root = self.children.get_mut(&mode)?;
        let removed = Trie::remove_path(root, keys);
        if matches!(root.as_ref(), Trie::Node { children, on_char: None } if children.is_empty()) {
            self.children.remove(&mode);
        }
        removed
    }
}

impl Default for KeymapRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Iterator over every binding across all modes in a [`KeymapRegistry`].
pub struct KeymapRegistryIter<'a> {
    modes: std::collections::hash_map::Iter<'a, Rc<str>, Rc<Trie>>,
    current_mode: Option<Rc<str>>,
    inner: Option<TrieIter<'a>>,
}

impl<'a> Iterator for KeymapRegistryIter<'a> {
    type Item = (Rc<str>, Vec<KeyEvent>, &'a Action);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(ref mut trie_iter) = self.inner
                && let Some((path, action)) = trie_iter.next()
            {
                let mode = self.current_mode.clone().expect("mode set with inner iter");
                return Some((mode, path, action));
            }
            let (mode, trie) = self.modes.next()?;
            self.current_mode = Some(mode.clone());
            self.inner = Some(trie.as_ref().into_iter());
        }
    }
}

impl<'a> IntoIterator for &'a KeymapRegistry {
    type Item = (Rc<str>, Vec<KeyEvent>, &'a Action);
    type IntoIter = KeymapRegistryIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        KeymapRegistryIter {
            modes: self.children.iter(),
            current_mode: None,
            inner: None,
        }
    }
}

impl KeymapRegistry {
    /// Returns a borrowing iterator over every `(mode, path, action)` triple
    /// registered across all modes.
    pub fn iter(&self) -> KeymapRegistryIter<'_> {
        self.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rizz_input::{KeyCode, KeyEvent};

    fn key(c: char) -> KeyEvent {
        KeyEvent::from_code(KeyCode::Char(c))
    }

    fn act(tag: &str) -> Rc<Action> {
        Rc::new(Action::EvalLisp(Rc::new(rizz::runtime::Value::Str(
            tag.into(),
        ))))
    }

    fn rhs(a: &Rc<Action>) -> String {
        match a.as_ref() {
            Action::EvalLisp(v) => v.as_str().map(|s| s.to_string()).unwrap_or_default(),
            _ => String::new(),
        }
    }

    #[test]
    fn upper_layer_shadows_lower_layer() {
        let mut r = KeymapRegistry::new();
        r.set("base".into(), &[key('q')], act("base-q"));
        r.set("top".into(), &[key('q')], act("top-q"));
        let modes: Vec<Rc<str>> = vec!["top".into(), "base".into()];
        let out = r.resolve(&modes, key('q'), false).expect("resolved");
        assert_eq!(rhs(&out[0]), "top-q");
    }

    #[test]
    fn unbound_in_upper_layer_falls_through_to_lower() {
        let mut r = KeymapRegistry::new();
        r.set("base".into(), &[key('j')], act("base-j"));
        r.set("top".into(), &[key('q')], act("top-q"));
        let modes: Vec<Rc<str>> = vec!["top".into(), "base".into()];
        let out = r.resolve(&modes, key('j'), false).expect("resolved");
        assert_eq!(rhs(&out[0]), "base-j");
    }

    #[test]
    fn sequence_continuation_only_when_mode_still_active() {
        let mut r = KeymapRegistry::new();
        r.set("top".into(), &[key('g'), key('g')], act("top-gg"));
        r.set("base".into(), &[key('g')], act("base-g"));
        let modes_top: Vec<Rc<str>> = vec!["top".into(), "base".into()];
        assert!(r.resolve(&modes_top, key('g'), false).is_none());
        let out = r.resolve(&modes_top, key('g'), false).expect("resolved");
        assert_eq!(rhs(&out[0]), "top-gg");

        assert!(r.resolve(&modes_top, key('g'), false).is_none());
        let modes_no_top: Vec<Rc<str>> = vec!["base".into()];
        let out = r.resolve(&modes_no_top, key('g'), false).expect("resolved");
        assert_eq!(rhs(&out[0]), "base-g");
    }

    fn insert_char_tag(a: &Rc<Action>) -> Option<char> {
        match a.as_ref() {
            Action::InsertChar(c) => Some(*c),
            _ => None,
        }
    }

    fn spec_char_tag(a: &Rc<Action>) -> Option<char> {
        match a.as_ref() {
            Action::SpeculativeInsertChar(c) => Some(*c),
            _ => None,
        }
    }

    #[test]
    fn descending_into_chord_speculates_via_on_char() {
        // First press of the `jk` escape chord: the descending mode is
        // `insert`, whose `on_char` would have produced `InsertChar('j')`,
        // so the keymap emits a `SpeculativeInsertChar('j')` immediately.
        let mut r = KeymapRegistry::new();
        r.set("insert".into(), &[key('j'), key('k')], act("esc"));

        let modes: Vec<Rc<str>> = vec!["insert".into()];
        let out = r
            .resolve(&modes, key('j'), false)
            .expect("speculative action emitted on descend");
        assert_eq!(out.len(), 1);
        assert_eq!(spec_char_tag(&out[0]), Some('j'));
        assert!(!r.is_idle(), "chord prefix is still in flight");
    }

    #[test]
    fn aborted_chord_commits_speculation_then_resolves_new_key() {
        // `jk` chord aborts because `x` doesn't continue it. The held `j`
        // was speculated, so it gets committed (not re-flushed via on_char)
        // and the fresh `x` resolves normally to InsertChar.
        let mut r = KeymapRegistry::new();
        r.set("insert".into(), &[key('j'), key('k')], act("esc"));

        let modes: Vec<Rc<str>> = vec!["insert".into()];
        assert_eq!(
            spec_char_tag(&r.resolve(&modes, key('j'), false).unwrap()[0]),
            Some('j'),
        );

        let out = r.resolve(&modes, key('x'), false).expect("resolved");
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0].as_ref(), Action::CommitSpeculation));
        assert_eq!(insert_char_tag(&out[1]), Some('x'));
    }

    #[test]
    fn timed_out_chord_commits_speculation_then_resolves_new_key() {
        // Same as above but the abandonment trigger is a timeout instead
        // of a non-matching key.
        let mut r = KeymapRegistry::new();
        r.set("insert".into(), &[key('j'), key('k')], act("esc"));

        let modes: Vec<Rc<str>> = vec!["insert".into()];
        assert_eq!(
            spec_char_tag(&r.resolve(&modes, key('j'), false).unwrap()[0]),
            Some('j'),
        );

        let out = r.resolve(&modes, key('a'), true).expect("resolved");
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0].as_ref(), Action::CommitSpeculation));
        assert_eq!(insert_char_tag(&out[1]), Some('a'));
    }

    #[test]
    fn completing_chord_rolls_back_speculation_before_action() {
        // The `j` was speculated; pressing `k` completes the chord, so the
        // staged speculation must be rolled back before `esc` runs.
        let mut r = KeymapRegistry::new();
        r.set("insert".into(), &[key('j'), key('k')], act("esc"));

        let modes: Vec<Rc<str>> = vec!["insert".into()];
        let _ = r.resolve(&modes, key('j'), false).unwrap();

        let out = r.resolve(&modes, key('k'), false).expect("chord completes");
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0].as_ref(), Action::RollbackSpeculation));
        assert_eq!(rhs(&out[1]), "esc");
        assert!(r.is_idle());
    }

    #[test]
    fn repeated_chord_prefix_commits_then_speculates_again() {
        // Typing `j` twice in a row: the first speculation commits (no
        // chord matched) and the second `j` starts a fresh speculation.
        let mut r = KeymapRegistry::new();
        r.set("insert".into(), &[key('j'), key('k')], act("esc"));

        let modes: Vec<Rc<str>> = vec!["insert".into()];
        let _ = r.resolve(&modes, key('j'), false).unwrap();

        let out = r
            .resolve(&modes, key('j'), false)
            .expect("first commits, second speculates");
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0].as_ref(), Action::CommitSpeculation));
        assert_eq!(spec_char_tag(&out[1]), Some('j'));
        assert!(!r.is_idle(), "second j is held as a new chord prefix");

        // Completing the chord still rolls back the second speculation.
        let out = r.resolve(&modes, key('k'), false).expect("chord completes");
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0].as_ref(), Action::RollbackSpeculation));
        assert_eq!(rhs(&out[1]), "esc");
    }

    #[test]
    fn descent_without_on_char_does_not_speculate() {
        // Mode has no `on_char`, so a held chord prefix can't be speculated.
        // The first key just descends silently; on abort, the prefix is
        // dropped (no Leaf, no on_char to flush through).
        let mut r = KeymapRegistry::new();
        r.set("layer".into(), &[key('j'), key('k')], act("jk"));

        let modes: Vec<Rc<str>> = vec!["layer".into()];
        assert!(r.resolve(&modes, key('j'), false).is_none());
        let out = r.resolve(&modes, key('x'), false);
        assert!(out.is_none());
        assert!(r.is_idle());
    }
}
