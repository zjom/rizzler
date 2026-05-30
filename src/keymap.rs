pub use crossterm::event::{KeyCode, KeyModifiers};

use crate::{action::Action, mode::EditingMode};
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}
impl KeyEvent {
    pub const fn from_code(code: KeyCode) -> Self {
        Self {
            code,
            modifiers: KeyModifiers::NONE,
        }
    }
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
    defaults: HashMap<EditingMode, Rc<Trie>>,
    cur: Option<Rc<Trie>>,
    prev_mode: Option<EditingMode>,
}

impl KeymapRegistry {
    pub fn new() -> Self {
        Self {
            children: HashMap::new(),
            defaults: build_defaults(),
            cur: None,
            prev_mode: None,
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
            _ => self.children.get(&mode).cloned(),
        };

        let user_action = match start.as_deref().map(|t| walk(t, key)) {
            Some(WalkOutcome::Action(a)) => Some(a),
            Some(WalkOutcome::Descend(n)) => {
                self.cur = Some(n);
                return None; // mid-sequence; defaults must not preempt
            }
            Some(WalkOutcome::Miss) | None => None,
        };

        user_action.or_else(|| {
            self.defaults.get(&mode).and_then(|d| match walk(d, key) {
                WalkOutcome::Action(a) => Some(a),
                _ => None,
            })
        })
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

#[derive(Debug, Clone)]
pub enum Trie {
    Node {
        children: HashMap<KeyEvent, Rc<Trie>>,
        /// Catches any `KeyCode::Char(_)` not bound in `children`, producing
        /// an action from the captured char. Terminal — does not descend.
        on_char: Option<fn(char) -> Action>,
    },
    Leaf(Action),
}
impl Trie {
    fn empty() -> Trie {
        Trie::Node {
            children: HashMap::new(),
            on_char: None,
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
                if let Trie::Node { children, .. } = km {
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
                let Trie::Node { children, .. } = Rc::make_mut(node) else {
                    return None;
                };
                match children.remove(last)?.as_ref() {
                    Trie::Leaf(action) => Some(action.clone()),
                    Trie::Node { .. } => None,
                }
            }
            [first, rest @ ..] => {
                let Trie::Node { children, .. } = Rc::make_mut(node) else {
                    return None;
                };
                let child = children.get_mut(first)?;
                let removed = Trie::remove_path(child, rest);
                // Prune the child if removing left it as an empty node.
                let prune = matches!(
                    child.as_ref(),
                    Trie::Node { children, on_char: None } if children.is_empty()
                );
                if prune {
                    children.remove(first);
                }
                removed
            }
        }
    }
}

enum WalkOutcome {
    Action(Action),
    Descend(Rc<Trie>),
    Miss,
}

/// Resolve one key against `trie`. Returns either the matched action, the
/// next subtree to wait on, or a miss. A `Trie::Leaf` root maps every key to
/// the same action; otherwise we look up `key` directly, then fall back to
/// the node's `on_char` wildcard if `key` is a printable char.
fn walk(trie: &Trie, key: KeyEvent) -> WalkOutcome {
    match trie {
        Trie::Leaf(a) => WalkOutcome::Action(a.clone()),
        Trie::Node { children, on_char } => {
            if let Some(next) = children.get(&key) {
                match next.as_ref() {
                    Trie::Leaf(a) => WalkOutcome::Action(a.clone()),
                    Trie::Node { .. } => WalkOutcome::Descend(next.clone()),
                }
            } else if let (Some(f), KeyCode::Char(c)) = (on_char, key.code) {
                WalkOutcome::Action(f(c))
            } else {
                WalkOutcome::Miss
            }
        }
    }
}

fn build_defaults() -> HashMap<EditingMode, Rc<Trie>> {
    let leaf = |a: Action| Rc::new(Trie::Leaf(a));
    let k = KeyEvent::from_code;

    let mv_down = leaf(Action::MoveCursor(0, 1));
    let mv_up = leaf(Action::MoveCursor(0, -1));
    let mv_left = leaf(Action::MoveCursor(-1, 0));
    let mv_right = leaf(Action::MoveCursor(1, 0));

    HashMap::from([
        (
            EditingMode::Command,
            Rc::new(Trie::Node {
                children: HashMap::from([
                    (k(KeyCode::Enter), leaf(Action::CommandSubmit)),
                    (k(KeyCode::Backspace), leaf(Action::CommandPop)),
                    (k(KeyCode::Esc), leaf(Action::CommandCancel)),
                ]),
                on_char: Some(Action::CommandPush),
            }),
        ),
        (
            EditingMode::Insert,
            Rc::new(Trie::Node {
                children: HashMap::from([
                    (k(KeyCode::Enter), leaf(Action::InsertNewline)),
                    (k(KeyCode::Backspace), leaf(Action::DeleteChar)),
                    (k(KeyCode::Esc), leaf(Action::SetMode(EditingMode::Normal))),
                ]),
                on_char: Some(Action::InsertChar),
            }),
        ),
        (
            EditingMode::Normal,
            Rc::new(Trie::Node {
                children: HashMap::from([
                    (
                        k(KeyCode::Char(':')),
                        leaf(Action::SetMode(EditingMode::Command)),
                    ),
                    (
                        k(KeyCode::Char('i')),
                        leaf(Action::SetMode(EditingMode::Insert)),
                    ),
                    (k(KeyCode::Char('j')), mv_down.clone()),
                    (k(KeyCode::Down), mv_down),
                    (k(KeyCode::Char('k')), mv_up.clone()),
                    (k(KeyCode::Up), mv_up),
                    (k(KeyCode::Char('h')), mv_left.clone()),
                    (k(KeyCode::Left), mv_left),
                    (k(KeyCode::Char('l')), mv_right.clone()),
                    (k(KeyCode::Right), mv_right),
                ]),
                on_char: None,
            }),
        ),
    ])
}
