use crate::action::MoveKind;
use crate::keymap::{KeyCode, KeyEvent, trie::Trie};
use crate::{action::Action, mode::EditingMode};
use std::collections::HashMap;
use std::rc::Rc;

pub fn defaults() -> HashMap<EditingMode, Rc<Trie>> {
    let leaf = |a: Action| Rc::new(Trie::Leaf(Rc::new(a)));
    let k = KeyEvent::from_code;

    let mv_down = leaf(Action::MoveCursor(MoveKind::Relative(0, 1)));
    let mv_up = leaf(Action::MoveCursor(MoveKind::Relative(0, -1)));
    let mv_left = leaf(Action::MoveCursor(MoveKind::Relative(-1, 0)));
    let mv_right = leaf(Action::MoveCursor(MoveKind::Relative(1, 0)));

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
