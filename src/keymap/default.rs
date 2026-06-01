use crate::action::Action;
use crate::keymap::trie::Trie;
use crate::mode::EditingMode;
use std::collections::HashMap;
use std::rc::Rc;

/// Seed only what the trie can't express through `keymap-set`: the `on_char`
/// fallthrough that lets insert/command modes accept typed characters without
/// an explicit binding per key. Every concrete keybinding is loaded from
/// `default.lisp` by `State::with_config`.
pub fn defaults() -> HashMap<Rc<str>, Rc<Trie>> {
    let typing_node = Rc::new(Trie::Node {
        children: HashMap::new(),
        on_char: Some(Action::InsertChar),
    });
    HashMap::from([
        (EditingMode::Insert.to_str().into(), typing_node.clone()),
        (EditingMode::Command.to_str().into(), typing_node),
    ])
}
