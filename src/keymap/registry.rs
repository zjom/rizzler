use crate::keymap::{
    KeyEvent,
    default::defaults,
    trie::{Trie, WalkOutcome, walk},
};
use crate::{action::Action, mode::EditingMode};
use std::collections::HashMap;
use std::rc::Rc;

pub struct KeymapRegistry {
    children: HashMap<EditingMode, Rc<Trie>>,
    cur: Option<Rc<Trie>>,
    prev_mode: Option<EditingMode>,
}

impl KeymapRegistry {
    pub fn new() -> Self {
        Self {
            children: defaults(),
            cur: None,
            prev_mode: None,
        }
    }
    pub fn resolve(&mut self, mode: EditingMode, key: KeyEvent) -> Option<Vec<Rc<Action>>> {
        // Continue an in-progress sequence only if the mode is unchanged;
        // otherwise restart from this mode's root keymap. `take()` clears
        // any stale sequence either way.
        let continuing = self.prev_mode.as_ref() == Some(&mode);
        self.prev_mode = Some(mode);

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
    pub fn set(&mut self, mode: EditingMode, keys: &[KeyEvent], action: Rc<Action>) {
        self.cur = None; // editing invalidates any partial sequence
        let root = self
            .children
            .entry(mode)
            .or_insert_with(|| Rc::new(Trie::empty()));
        Trie::insert_path(root, keys, action);
    }

    /// Remove the binding at `keys` in `mode`, returning the removed action if
    /// it was a leaf. Drops the mode entirely if its root becomes empty.
    pub fn remove(&mut self, mode: EditingMode, keys: &[KeyEvent]) -> Option<Rc<Action>> {
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
