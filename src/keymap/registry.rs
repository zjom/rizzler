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
    pub fn resolve(
        &mut self,
        mode: Rc<str>,
        key: KeyEvent,
        timedout: bool,
    ) -> Option<Vec<Rc<Action>>> {
        // Continue an in-progress sequence only if the mode is unchanged;
        // otherwise restart from this mode's root keymap. `take()` clears
        // any stale sequence either way.
        let continuing = !timedout && self.prev_mode.as_ref() == Some(&mode);
        self.prev_mode = Some(mode.clone());

        let start = match self.cur.take() {
            Some(cur) if continuing => Some(cur),
            _ => self.children.get(&mode).cloned(),
        };

        let user_action = match start.as_deref().map(|t| walk(t, key)) {
            Some(WalkOutcome::Action(a)) => Some(a),
            Some(WalkOutcome::Descend(n)) => {
                self.cur = Some(n);
                return None;
            }
            Some(WalkOutcome::Miss) | None => None,
        };

        user_action.map(|a| vec![a])
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
