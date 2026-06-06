use crate::action::Action;
use crate::keymap::trie::TrieIter;
use crate::keymap::{
    KeyEvent,
    default::defaults,
    trie::{Trie, WalkOutcome, walk},
};
use std::collections::HashMap;
use std::rc::Rc;

pub struct KeymapRegistry {
    children: HashMap<Rc<str>, Rc<Trie>>,
    cur: Option<Rc<Trie>>,
    prev_mode: Option<Rc<str>>,
}

impl KeymapRegistry {
    pub fn new() -> Self {
        Self {
            children: defaults(),
            cur: None,
            prev_mode: None,
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
    /// `Action` or `Descend` wins. An in-progress sequence is only
    /// continued if its mode is still in `modes` and `timedout` is false;
    /// otherwise resolution restarts from each mode's root.
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
                    self.prev_mode = None;
                    return Some(vec![a]);
                }
                WalkOutcome::Descend(n) => {
                    self.cur = Some(n);
                    return None;
                }
                WalkOutcome::Miss => {
                    // Drop the stale sequence; fall through to a fresh
                    // top-level resolution so the user's key still has a
                    // chance to match a different layer.
                }
            }
        }
        self.cur = None;
        self.prev_mode = None;

        for mode in modes {
            let Some(root) = self.children.get(mode).cloned() else {
                continue;
            };
            match walk(&root, key) {
                WalkOutcome::Action(a) => {
                    return Some(vec![a]);
                }
                WalkOutcome::Descend(n) => {
                    self.cur = Some(n);
                    self.prev_mode = Some(mode.clone());
                    return None;
                }
                WalkOutcome::Miss => {}
            }
        }
        None
    }

    /// Bind a key sequence in `mode` to `action`.
    pub fn set(&mut self, mode: Rc<str>, keys: &[KeyEvent], action: Rc<Action>) {
        self.cur = None; // editing invalidates any partial sequence
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
///
/// Each item is `(mode, path, action)`.  Modes are visited in unspecified
/// order; bindings within a mode follow [`TrieIter`] ordering (also
/// unspecified).
pub struct KeymapRegistryIter<'a> {
    /// Outer cursor — advances to the next mode once the inner one is drained.
    modes: std::collections::hash_map::Iter<'a, Rc<str>, Rc<Trie>>,
    /// Mode name for whatever `inner` is currently walking.
    current_mode: Option<Rc<str>>,
    /// Inner cursor — walks bindings within the current mode's trie.
    inner: Option<TrieIter<'a>>,
}

impl<'a> Iterator for KeymapRegistryIter<'a> {
    type Item = (Rc<str>, Vec<KeyEvent>, &'a Action);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            // Drain the current trie before moving to the next mode.
            if let Some(ref mut trie_iter) = self.inner
                && let Some((path, action)) = trie_iter.next()
            {
                // `current_mode` is always `Some` while `inner` is `Some`.
                let mode = self.current_mode.clone().expect("mode set with inner iter");
                return Some((mode, path, action));
            }
            // Current trie exhausted (or not yet started) — advance to next mode.
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
    use crate::keymap::{KeyCode, KeyEvent};

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
        // `gg` in `top` descends after the first `g`. If the next call
        // includes `top` in `modes`, the second `g` completes the
        // sequence; if `top` is no longer active, the partial sequence
        // is dropped before retrying against the new modes.
        let mut r = KeymapRegistry::new();
        r.set("top".into(), &[key('g'), key('g')], act("top-gg"));
        r.set("base".into(), &[key('g')], act("base-g"));
        let modes_top: Vec<Rc<str>> = vec!["top".into(), "base".into()];
        assert!(r.resolve(&modes_top, key('g'), false).is_none());
        let out = r.resolve(&modes_top, key('g'), false).expect("resolved");
        assert_eq!(rhs(&out[0]), "top-gg");

        // Restart a sequence, then drop `top` before the second key.
        assert!(r.resolve(&modes_top, key('g'), false).is_none());
        let modes_no_top: Vec<Rc<str>> = vec!["base".into()];
        let out = r.resolve(&modes_no_top, key('g'), false).expect("resolved");
        // Stale `top` sequence dropped; `g` re-resolves against `base`.
        assert_eq!(rhs(&out[0]), "base-g");
    }
}
