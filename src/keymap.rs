pub use crossterm::event::{KeyCode, KeyModifiers};

use crate::{action::Action, mode::EditingMode};
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}
impl From<crossterm::event::KeyEvent> for KeyEvent {
    fn from(value: crossterm::event::KeyEvent) -> Self {
        Self {
            code: value.code,
            modifiers: value.modifiers,
        }
    }
}

pub struct KeymapRegistry {
    children: HashMap<EditingMode, Rc<Trie>>,
    cur: Option<Rc<Trie>>,
    prev_mode: Option<EditingMode>,
    default_km: DefaultKeymap,
}

impl KeymapRegistry {
    pub fn new() -> Self {
        Self {
            children: HashMap::new(),
            cur: None,
            prev_mode: None,
            default_km: DefaultKeymap,
        }
    }
    pub fn resolve(&mut self, mode: EditingMode, key: KeyEvent) -> Option<Action> {
        // Continue an in-progress sequence only if the mode is unchanged;
        // otherwise restart from this mode's root keymap. `take()` clears
        // any stale sequence either way.
        let continuing = self.prev_mode.as_ref() == Some(&mode);
        self.prev_mode = Some(mode);

        let start = match self.cur.take() {
            Some(cur) if continuing => Some(cur),
            _ => self.children.get(&mode).map(Rc::clone),
        };

        let user_action = match start.as_deref() {
            // A mode root that is itself a leaf maps every key to one action.
            Some(Trie::Leaf(action)) => Some(action.clone()),
            Some(Trie::Node { .. }) => match start.as_ref().unwrap().child(key) {
                Some(next) => match next.as_ref() {
                    Trie::Leaf(action) => Some(action.clone()),
                    Trie::Node { .. } => {
                        self.cur = Some(next);
                        return None; // mid-sequence; defaults must not preempt
                    }
                },
                None => None,
            },
            None => None,
        };

        user_action.or_else(|| self.default_km.resolve(mode, key))
    }

    /// Bind a key sequence in `mode` to `action`.
    pub fn set(&mut self, mode: EditingMode, keys: &[KeyEvent], action: Action) {
        self.cur = None; // editing invalidates any partial sequence
        let root = self
            .children
            .entry(mode)
            .or_insert_with(|| Rc::new(Trie::empty()));
        Trie::insert_path(root, keys, action);
    }

    /// Remove the binding at `keys` in `mode`, returning the removed action if
    /// it was a leaf. Drops the mode entirely if its root becomes empty.
    pub fn remove(&mut self, mode: EditingMode, keys: &[KeyEvent]) -> Option<Action> {
        self.cur = None;
        let root = self.children.get_mut(&mode)?;
        let removed = Trie::remove_path(root, keys);
        if matches!(root.as_ref(), Trie::Node { children } if children.is_empty()) {
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

#[derive(Debug, Clone)]
pub enum Trie {
    Node {
        children: HashMap<KeyEvent, Rc<Trie>>,
    },
    Leaf(Action),
}
impl Trie {
    /// Descend one level by `key`. Returns the child keymap, or `None`
    /// if this is a leaf or the key isn't bound.
    fn child(&self, key: KeyEvent) -> Option<Rc<Trie>> {
        match self {
            Trie::Leaf(_) => None,
            Trie::Node { children } => children.get(&key).cloned(),
        }
    }

    fn empty() -> Trie {
        Trie::Node {
            children: HashMap::new(),
        }
    }

    /// Bind `keys` to `action` under this node, creating intermediate nodes
    /// as needed. A conflicting binding on the same path is overwritten
    /// (last write wins).
    fn insert_path(node: &mut Rc<Trie>, keys: &[KeyEvent], action: Action) {
        match keys {
            [] => {
                // End of the path: this slot becomes the action.
                *node = Rc::new(Trie::Leaf(action));
            }
            [first, rest @ ..] => {
                let km = Rc::make_mut(node);
                // A path can only continue through a node; promote a leaf.
                if !matches!(km, Trie::Node { .. }) {
                    *km = Trie::empty();
                }
                if let Trie::Node { children } = km {
                    let child = children
                        .entry(*first)
                        .or_insert_with(|| Rc::new(Trie::empty()));
                    Trie::insert_path(child, rest, action);
                }
            }
        }
    }

    /// Remove the binding at `keys`. Returns the action if the path pointed at
    /// a leaf (if it pointed at an intermediate node, the whole subtree is
    /// removed and `None` is returned). Empty intermediate nodes are pruned.
    fn remove_path(node: &mut Rc<Trie>, keys: &[KeyEvent]) -> Option<Action> {
        match keys {
            [] => None, // can't remove the root through this API
            [last] => {
                let Trie::Node { children } = Rc::make_mut(node) else {
                    return None;
                };
                match children.remove(last)?.as_ref() {
                    Trie::Leaf(action) => Some(action.clone()),
                    Trie::Node { .. } => None,
                }
            }
            [first, rest @ ..] => {
                let Trie::Node { children } = Rc::make_mut(node) else {
                    return None;
                };
                let child = children.get_mut(first)?;
                let removed = Trie::remove_path(child, rest);
                // Prune the child if removing left it as an empty node.
                let prune =
                    matches!(child.as_ref(), Trie::Node { children } if children.is_empty());
                if prune {
                    children.remove(first);
                }
                removed
            }
        }
    }
}

pub struct DefaultKeymap;
impl DefaultKeymap {
    fn resolve(&mut self, mode: EditingMode, key: KeyEvent) -> Option<Action> {
        match mode {
            EditingMode::Command => match key.code {
                KeyCode::Enter => Some(Action::CommandSubmit),
                KeyCode::Char(c) => Some(Action::CommandPush(c)),
                KeyCode::Backspace => Some(Action::CommandPop),
                KeyCode::Esc => Some(Action::CommandCancel),
                _ => None,
            },
            EditingMode::Insert => match key.code {
                KeyCode::Enter => Some(Action::InsertNewline),
                KeyCode::Char(c) => Some(Action::InsertChar(c)),
                KeyCode::Backspace => Some(Action::DeleteChar),
                KeyCode::Esc => Some(Action::SetMode(EditingMode::Normal)),
                _ => None,
            },
            EditingMode::Normal => match key.code {
                KeyCode::Char(':') => Some(Action::SetMode(EditingMode::Command)),
                KeyCode::Char('i') => Some(Action::SetMode(EditingMode::Insert)),
                KeyCode::Char('j') | KeyCode::Down => Some(Action::MoveCursor(0, 1)),
                KeyCode::Char('k') | KeyCode::Up => Some(Action::MoveCursor(0, -1)),
                KeyCode::Char('h') | KeyCode::Left => Some(Action::MoveCursor(-1, 0)),
                KeyCode::Char('l') | KeyCode::Right => Some(Action::MoveCursor(1, 0)),
                _ => None,
            },
            EditingMode::Visual => None,
        }
    }
}
