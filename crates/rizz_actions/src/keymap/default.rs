use std::collections::HashMap;
use std::rc::Rc;

use rizz_core::EditingMode;

use crate::action::Action;
use crate::keymap::trie::Trie;

/// Seed only what the trie can't express through `keymap-set`: the `on_char`
/// fallthrough that lets insert/command modes accept typed characters without
/// an explicit binding per key. Every concrete keybinding is loaded from
/// `default.lisp` by `State::with_config`.
pub fn default_keymaps() -> HashMap<Rc<str>, Rc<Trie>> {
    let typing_node = Rc::new(Trie::Node {
        children: HashMap::new(),
        on_char: Some(Action::InsertChar),
    });
    HashMap::from([
        (EditingMode::Insert.as_str().into(), typing_node.clone()),
        (EditingMode::Command.as_str().into(), typing_node),
    ])
}
