//! Pure helpers for command-line tab completion. Locates the symbol-like
//! token under the cursor and computes the longest common prefix among
//! candidate strings — both feed the lisp builtins in
//! `lisp::builtins::minibuffer`.
//!
//! A "word" to complete is a run of non-whitespace, non-delimiter chars.
//! The exclusion set covers lisp surface syntax (parens, brackets, quotes,
//! comment introducer) so completing `(ed|it` only replaces `edit`.

fn is_word_char(c: char) -> bool {
    !c.is_whitespace()
        && !matches!(
            c,
            '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '`' | ',' | ';'
        )
}

/// Char-indexed `[start, end)` range of the token spanning `cursor`.
/// Walks both directions from `cursor` while [`is_word_char`] holds.
/// When the cursor isn't adjacent to a word char both bounds collapse to
/// `cursor`, so `[start..cursor]` and `[cursor..end]` are both empty.
pub fn token_bounds(text: &str, cursor: usize) -> (usize, usize) {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let cur = cursor.min(n);
    let mut start = cur;
    while start > 0 && is_word_char(chars[start - 1]) {
        start -= 1;
    }
    let mut end = cur;
    while end < n && is_word_char(chars[end]) {
        end += 1;
    }
    (start, end)
}

/// Substring of the token spanning `cursor`, from the token's left boundary
/// up to (but not including) `cursor`. This is what candidates must
/// `starts_with` to be eligible completions.
pub fn prefix_at(text: &str, cursor: usize) -> String {
    let (start, _) = token_bounds(text, cursor);
    text.chars()
        .skip(start)
        .take(cursor.saturating_sub(start))
        .collect()
}

/// Longest prefix shared by every string in `candidates`, counted by char.
/// Empty input — or any candidate being empty — yields `""`.
pub fn longest_common_prefix<I, S>(candidates: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut iter = candidates.into_iter();
    let Some(first) = iter.next() else {
        return String::new();
    };
    let mut prefix: Vec<char> = first.as_ref().chars().collect();
    for s in iter {
        let limit = prefix.len();
        let mut new_len = 0;
        for (i, c) in s.as_ref().chars().take(limit).enumerate() {
            if prefix[i] != c {
                break;
            }
            new_len = i + 1;
        }
        prefix.truncate(new_len);
        if prefix.is_empty() {
            break;
        }
    }
    prefix.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_bounds_in_middle_of_word() {
        let (s, e) = token_bounds("edit foo", 2);
        assert_eq!((s, e), (0, 4));
    }

    #[test]
    fn token_bounds_at_word_end() {
        let (s, e) = token_bounds("edit foo", 4);
        assert_eq!((s, e), (0, 4));
    }

    #[test]
    fn token_bounds_inside_paren_form() {
        let (s, e) = token_bounds("(edit foo)", 3);
        assert_eq!((s, e), (1, 5));
    }

    #[test]
    fn token_bounds_after_whitespace_is_empty() {
        let (s, e) = token_bounds("edit ", 5);
        assert_eq!((s, e), (5, 5));
    }

    #[test]
    fn token_bounds_after_paren_is_empty() {
        let (s, e) = token_bounds("(edit)", 6);
        assert_eq!((s, e), (6, 6));
    }

    #[test]
    fn token_bounds_clamps_past_end() {
        let (s, e) = token_bounds("hi", 999);
        assert_eq!((s, e), (0, 2));
    }

    #[test]
    fn prefix_at_takes_up_to_cursor() {
        assert_eq!(prefix_at("edit foo", 2), "ed");
        assert_eq!(prefix_at("edit foo", 4), "edit");
        assert_eq!(prefix_at("(insert", 5), "inse");
    }

    #[test]
    fn longest_common_prefix_basic() {
        assert_eq!(longest_common_prefix(["edit", "editor", "edits"]), "edit");
    }

    #[test]
    fn longest_common_prefix_diverges_immediately() {
        assert_eq!(longest_common_prefix(["abc", "xyz"]), "");
    }

    #[test]
    fn longest_common_prefix_single_candidate() {
        assert_eq!(longest_common_prefix(["solo"]), "solo");
    }

    #[test]
    fn longest_common_prefix_empty_input() {
        assert_eq!(longest_common_prefix::<_, &str>([]), "");
    }

    #[test]
    fn longest_common_prefix_one_is_prefix_of_other() {
        assert_eq!(longest_common_prefix(["foo", "foobar"]), "foo");
    }
}
