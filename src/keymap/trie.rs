use crate::action::Action;
use crate::keymap::{KeyCode, KeyEvent};
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Debug, Clone)]
pub enum Trie {
    Node {
        children: HashMap<KeyEvent, Rc<Trie>>,
        /// Catches any `KeyCode::Char(_)` not bound in `children`, producing
        /// an action from the captured char. Terminal — does not descend.
        on_char: Option<fn(char) -> Action>,
    },
    Leaf(Rc<Action>),
}
impl Trie {
    pub fn empty() -> Trie {
        Trie::Node {
            children: HashMap::new(),
            on_char: None,
        }
    }

    /// Bind `keys` to `action` under this node, creating intermediate nodes
    /// as needed. A conflicting binding on the same path is overwritten
    /// (last write wins).
    pub fn insert_path(node: &mut Rc<Trie>, keys: &[KeyEvent], action: Rc<Action>) {
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
    pub fn remove_path(node: &mut Rc<Trie>, keys: &[KeyEvent]) -> Option<Rc<Action>> {
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

pub enum WalkOutcome {
    Action(Rc<Action>),
    Descend(Rc<Trie>),
    Miss,
}

/// Resolve one key against `trie`. Returns either the matched action, the
/// next subtree to wait on, or a miss. A `Trie::Leaf` root maps every key to
/// the same action; otherwise we look up `key` directly, then fall back to
/// the node's `on_char` wildcard if `key` is a printable char.
pub fn walk(trie: &Trie, key: KeyEvent) -> WalkOutcome {
    match trie {
        Trie::Leaf(a) => WalkOutcome::Action(a.clone()),
        Trie::Node { children, on_char } => {
            if let Some(next) = children.get(&key) {
                match next.as_ref() {
                    Trie::Leaf(a) => WalkOutcome::Action(a.clone()),
                    Trie::Node { .. } => WalkOutcome::Descend(next.clone()),
                }
            } else if let (Some(f), KeyCode::Char(c)) = (on_char, key.code) {
                WalkOutcome::Action(f(c).into())
            } else {
                WalkOutcome::Miss
            }
        }
    }
}
