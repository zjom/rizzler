use crate::buffer::MoveKind as MK;
use crate::keymap::{KeyCode as KC, KeyEvent, KeyModifiers, trie::Trie};
use crate::position::Position;
use crate::window::{FocusDir, SplitDir};
use crate::{action::Action as A, mode::EditingMode};
use std::collections::HashMap;
use std::rc::Rc;

pub fn defaults() -> HashMap<EditingMode, Rc<Trie>> {
    let leaf = |a: A| Rc::new(Trie::Leaf(Rc::new(a)));
    let k = KeyEvent::from_code;
    let ctrl = |c: char| KeyEvent {
        code: KC::Char(c),
        modifiers: KeyModifiers::CONTROL,
    };
    let mv = |k: MK| leaf(A::MoveCursor(k));

    let mv_down = mv(MK::Relative(Position::new(0, 1)));
    let mv_up = mv(MK::Relative(Position::new(0, -1)));
    let mv_left = mv(MK::Relative(Position::new(-1, 0)));
    let mv_right = mv(MK::Relative(Position::new(1, 0)));

    HashMap::from([
        (
            EditingMode::Command,
            Rc::new(Trie::Node {
                children: HashMap::from([
                    (k(KC::Enter), leaf(A::CommandSubmit)),
                    (k(KC::Backspace), leaf(A::DeleteChar)),
                    (k(KC::Esc), leaf(A::CommandCancel)),
                ]),
                on_char: Some(A::InsertChar),
            }),
        ),
        (
            EditingMode::Insert,
            Rc::new(Trie::Node {
                children: HashMap::from([
                    (k(KC::Enter), leaf(A::InsertNewline)),
                    (k(KC::Backspace), leaf(A::DeleteChar)),
                    (k(KC::Esc), leaf(A::SetMode(EditingMode::Normal))),
                ]),
                on_char: Some(A::InsertChar),
            }),
        ),
        (
            EditingMode::Normal,
            Rc::new(Trie::Node {
                children: HashMap::from([
                    (k(KC::Char(':')), leaf(A::SetMode(EditingMode::Command))),
                    (k(KC::Char('i')), leaf(A::SetMode(EditingMode::Insert))),
                    (k(KC::Char('j')), mv_down.clone()),
                    (k(KC::Down), mv_down),
                    (k(KC::Char('k')), mv_up.clone()),
                    (k(KC::Up), mv_up),
                    (k(KC::Char('h')), mv_left.clone()),
                    (k(KC::Left), mv_left),
                    (k(KC::Char('l')), mv_right.clone()),
                    (k(KC::Right), mv_right),
                    (k(KC::Char('0')), mv(MK::LineStart)),
                    (k(KC::Char('$')), mv(MK::LineEnd)),
                    (
                        k(KC::Char('g')),
                        Rc::new(Trie::Node {
                            children: HashMap::from([(k(KC::Char('g')), mv(MK::FileStart))]),
                            on_char: None,
                        }),
                    ),
                    (k(KC::Char('G')), mv(MK::FileEnd)),
                    (k(KC::Char('b')), mv(MK::WordStart)),
                    (k(KC::Char('e')), mv(MK::WordEnd)),
                    (ctrl('d'), mv(MK::HalfPageDown)),
                    (ctrl('u'), mv(MK::HalfPageUp)),
                    (
                        k(KC::Char('z')),
                        Rc::new(Trie::Node {
                            children: HashMap::from([(k(KC::Char('z')), mv(MK::Center))]),
                            on_char: None,
                        }),
                    ),
                    (
                        ctrl('w'),
                        Rc::new(Trie::Node {
                            children: HashMap::from([
                                (k(KC::Char('q')), leaf(A::WindowClose)),
                                (k(KC::Char('"')), leaf(A::WindowSplit(SplitDir::Vertical))),
                                (k(KC::Char('|')), leaf(A::WindowSplit(SplitDir::Horizontal))),
                                (k(KC::Char('h')), leaf(A::WindowFocus(FocusDir::Left))),
                                (k(KC::Char('l')), leaf(A::WindowFocus(FocusDir::Right))),
                                (k(KC::Char('k')), leaf(A::WindowFocus(FocusDir::Up))),
                                (k(KC::Char('j')), leaf(A::WindowFocus(FocusDir::Down))),
                                (k(KC::Char('w')), leaf(A::WindowFocusNext)),
                            ]),
                            on_char: None,
                        }),
                    ),
                ]),
                on_char: None,
            }),
        ),
    ])
}
