use std::collections::HashMap;
use std::rc::Rc;

use rizz_core::EditingMode;
use rizz_input::{KeyCode, KeyEvent};

use crate::action::Action;
use crate::keymap::trie::Trie;

/// Seed only what the trie can't express through `keymap-set`: the `on_char`
/// fallthrough that lets a mode accept arbitrary typed characters without an
/// explicit binding per key, and the vim `r<char>` prefix that descends into
/// a one-key `on_char` capture. Everything else (the concrete keybindings
/// for normal / visual / insert mode) is loaded from `init.rz`.
pub fn default_keymaps() -> HashMap<Rc<str>, Rc<Trie>> {
    let typing_node = Rc::new(Trie::Node {
        children: HashMap::new(),
        on_char: Some(Action::InsertChar),
    });
    // `r` descends into a node whose `on_char` turns the next key into a
    // `ReplaceChar` — vim's `r<char>` "replace one char under cursor".
    // Sub-node `on_char` isn't expressible through `keymap-set`, so this
    // one binding has to live here.
    let normal_node = Rc::new(Trie::Node {
        children: HashMap::from([(
            KeyEvent::from_code(KeyCode::Char('r')),
            Rc::new(Trie::Node {
                children: HashMap::new(),
                on_char: Some(Action::ReplaceChar),
            }),
        )]),
        on_char: None,
    });
    // Replace mode: each typed char overwrites + advances. Explicit
    // bindings (`<esc>` -> normal, `<backspace>`, `<enter>`) layer on top
    // from `init.rz`.
    let overwrite_node = Rc::new(Trie::Node {
        children: HashMap::new(),
        on_char: Some(Action::OverwriteChar),
    });
    HashMap::from([
        (EditingMode::Insert.as_str().into(), typing_node.clone()),
        (EditingMode::Command.as_str().into(), typing_node),
        (EditingMode::Normal.as_str().into(), normal_node),
        (EditingMode::Replace.as_str().into(), overwrite_node),
    ])
}
