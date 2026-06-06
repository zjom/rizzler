//! Word motions (`w`, `b`, `e`, `ge` and their `W`/`B`/`E`/`gE` "big-word"
//! variants). Each motion is a pure function over `(rope, current absolute
//! char index) → new absolute char index`. Newlines act as whitespace in
//! every flavor.

use ropey::Rope;

/// Vim-style character class. `Word` matches `\w` (alphanumeric +
/// underscore), `Punct` is any other non-whitespace char. In "big-word" mode
/// (`W`/`B`/`E`/`gE`) `Word` and `Punct` collapse into one class.
#[derive(PartialEq, Eq, Copy, Clone)]
enum CharClass {
    Ws,
    Word,
    Punct,
}

fn char_class(c: char, big: bool) -> CharClass {
    if c.is_whitespace() {
        CharClass::Ws
    } else if big || c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Punct
    }
}

/// Vim `w`/`W` — start of the next word.
pub fn word_forward(rope: &Rope, cidx: usize, big: bool) -> usize {
    let len = rope.len_chars();
    if len == 0 || cidx >= len {
        return cidx;
    }
    let mut i = cidx;
    let start_class = char_class(rope.char(i), big);
    if start_class != CharClass::Ws {
        while i < len && char_class(rope.char(i), big) == start_class {
            i += 1;
        }
    }
    while i < len && char_class(rope.char(i), big) == CharClass::Ws {
        i += 1;
    }
    if i >= len { len - 1 } else { i }
}

/// Vim `b`/`B` — start of the word at or before the cursor.
pub fn word_back_start(rope: &Rope, cidx: usize, big: bool) -> usize {
    if cidx == 0 {
        return 0;
    }
    let mut i = cidx - 1;
    while i > 0 && char_class(rope.char(i), big) == CharClass::Ws {
        i -= 1;
    }
    let cls = char_class(rope.char(i), big);
    if cls != CharClass::Ws {
        while i > 0 && char_class(rope.char(i - 1), big) == cls {
            i -= 1;
        }
    }
    i
}

/// Vim `e`/`E` — end of the word at or after the cursor.
pub fn word_end(rope: &Rope, cidx: usize, big: bool) -> usize {
    let len = rope.len_chars();
    if len == 0 || cidx + 1 >= len {
        return cidx;
    }
    let mut i = cidx + 1;
    while i < len && char_class(rope.char(i), big) == CharClass::Ws {
        i += 1;
    }
    if i >= len {
        return len - 1;
    }
    let cls = char_class(rope.char(i), big);
    while i + 1 < len && char_class(rope.char(i + 1), big) == cls {
        i += 1;
    }
    i
}

/// Vim `ge`/`gE` — end of the previous word.
pub fn word_back_end(rope: &Rope, cidx: usize, big: bool) -> usize {
    if cidx == 0 {
        return 0;
    }
    let len = rope.len_chars();
    let mut i = cidx;
    if i < len {
        let cls = char_class(rope.char(i), big);
        if cls != CharClass::Ws {
            while i > 0 && char_class(rope.char(i - 1), big) == cls {
                i -= 1;
            }
        }
    }
    if i == 0 {
        return 0;
    }
    i -= 1;
    while i > 0 && char_class(rope.char(i), big) == CharClass::Ws {
        i -= 1;
    }
    i
}
