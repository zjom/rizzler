pub use crossterm::event::{KeyCode, KeyModifiers};

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

    /// Parse a key-sequence string into a list of `KeyEvent`s.
    ///
    /// Plain characters map to themselves: `"gg"` → two `g` events. Bracketed
    /// tokens name special keys or chords: `"<esc>"`, `"<c-w>q"`, `"<s-tab>"`.
    /// Recognised modifiers are `c-`/`s-`/`a-` (ctrl/shift/alt); named keys are
    /// `esc`, `enter`/`cr`, `backspace`/`bs`, `tab`, `space`, `up`/`down`/
    /// `left`/`right`, `home`/`end`, `pageup`/`pagedown`, and `lt` (literal `<`).
    pub fn parse_sequence(s: &str) -> Result<Vec<Self>, String> {
        let mut out = Vec::new();
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'<' {
                let end = s[i + 1..]
                    .find('>')
                    .ok_or_else(|| format!("unclosed `<` in key sequence: {s:?}"))?;
                let token = &s[i + 1..i + 1 + end];
                out.push(parse_token(token)?);
                i += 1 + end + 1;
            } else {
                let ch = s[i..].chars().next().unwrap();
                out.push(KeyEvent::from_code(KeyCode::Char(ch)));
                i += ch.len_utf8();
            }
        }
        Ok(out)
    }
}

fn parse_token(tok: &str) -> Result<KeyEvent, String> {
    let mut modifiers = KeyModifiers::NONE;
    let mut rest = tok;
    loop {
        let lower = rest.to_ascii_lowercase();
        if let Some(tail) = lower.strip_prefix("c-") {
            modifiers |= KeyModifiers::CONTROL;
            rest = &rest[rest.len() - tail.len()..];
        } else if let Some(tail) = lower.strip_prefix("s-") {
            modifiers |= KeyModifiers::SHIFT;
            rest = &rest[rest.len() - tail.len()..];
        } else if let Some(tail) = lower.strip_prefix("a-") {
            modifiers |= KeyModifiers::ALT;
            rest = &rest[rest.len() - tail.len()..];
        } else {
            break;
        }
    }
    let code = match rest.to_ascii_lowercase().as_str() {
        "esc" => KeyCode::Esc,
        "enter" | "cr" | "return" => KeyCode::Enter,
        "backspace" | "bs" => KeyCode::Backspace,
        "tab" => KeyCode::Tab,
        "space" => KeyCode::Char(' '),
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "lt" => KeyCode::Char('<'),
        "gt" => KeyCode::Char('>'),
        other if other.chars().count() == 1 => KeyCode::Char(rest.chars().next().unwrap()),
        _ => return Err(format!("unknown key token: <{tok}>")),
    };
    // Match the same SHIFT-stripping rule used by `From<crossterm::KeyEvent>`
    // so that synthesised and user-typed events compare equal in the keymap.
    if matches!(code, KeyCode::Char(_)) {
        modifiers.remove(KeyModifiers::SHIFT);
    }
    Ok(KeyEvent { code, modifiers })
}

impl From<crossterm::event::KeyEvent> for KeyEvent {
    fn from(value: crossterm::event::KeyEvent) -> Self {
        let mut modifiers = value.modifiers;
        if matches!(value.code, KeyCode::Char(_)) {
            modifiers.remove(KeyModifiers::SHIFT);
        }
        Self {
            code: value.code,
            modifiers,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_chars() {
        let seq = KeyEvent::parse_sequence("gg").unwrap();
        assert_eq!(seq.len(), 2);
        assert_eq!(seq[0], KeyEvent::from_code(KeyCode::Char('g')));
        assert_eq!(seq[1], KeyEvent::from_code(KeyCode::Char('g')));
    }

    #[test]
    fn parses_named_keys() {
        let seq = KeyEvent::parse_sequence("<esc>").unwrap();
        assert_eq!(seq, vec![KeyEvent::from_code(KeyCode::Esc)]);
        let seq = KeyEvent::parse_sequence("<enter>").unwrap();
        assert_eq!(seq, vec![KeyEvent::from_code(KeyCode::Enter)]);
    }

    #[test]
    fn parses_ctrl_chord() {
        let seq = KeyEvent::parse_sequence("<c-w>q").unwrap();
        assert_eq!(seq.len(), 2);
        assert_eq!(
            seq[0],
            KeyEvent {
                code: KeyCode::Char('w'),
                modifiers: KeyModifiers::CONTROL,
            }
        );
        assert_eq!(seq[1], KeyEvent::from_code(KeyCode::Char('q')));
    }

    #[test]
    fn parses_literal_lt() {
        let seq = KeyEvent::parse_sequence("<lt>").unwrap();
        assert_eq!(seq, vec![KeyEvent::from_code(KeyCode::Char('<'))]);
    }

    #[test]
    fn rejects_unknown_token() {
        assert!(KeyEvent::parse_sequence("<nope>").is_err());
    }

    #[test]
    fn rejects_unclosed_bracket() {
        assert!(KeyEvent::parse_sequence("<esc").is_err());
    }
}
