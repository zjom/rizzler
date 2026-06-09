//! Convert between rizzler's UTF-8 byte/char positions and LSP positions.
//!
//! LSP positions are `(line, character)`. The `character` field counts code
//! units in the negotiated encoding — UTF-16 by default, UTF-8 / UTF-32 if
//! the server agrees during `initialize`. Rizzler stores positions as
//! `(row, col)` where `col` is a UTF-8 byte offset into the line; we
//! translate at the boundary.

use lsp_types::{Position, PositionEncodingKind};
use ropey::Rope;

/// Encoding the client picked at `initialize` time.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default)]
pub enum Encoding {
    /// Default if the server omits `positionEncoding` from its capabilities.
    #[default]
    Utf16,
    Utf8,
    Utf32,
}

impl Encoding {
    pub fn from_lsp(kind: Option<&PositionEncodingKind>) -> Self {
        match kind {
            Some(k) if *k == PositionEncodingKind::UTF8 => Encoding::Utf8,
            Some(k) if *k == PositionEncodingKind::UTF32 => Encoding::Utf32,
            _ => Encoding::Utf16,
        }
    }

    pub fn to_lsp(self) -> PositionEncodingKind {
        match self {
            Encoding::Utf8 => PositionEncodingKind::UTF8,
            Encoding::Utf16 => PositionEncodingKind::UTF16,
            Encoding::Utf32 => PositionEncodingKind::UTF32,
        }
    }
}

/// Convert a (row, byte_col) coordinate into an LSP `Position` in the
/// negotiated `encoding`. `byte_col` is interpreted as the byte offset
/// from the start of `row`.
pub fn byte_to_lsp(rope: &Rope, row: usize, byte_col: usize, encoding: Encoding) -> Position {
    let row = row.min(rope.len_lines().saturating_sub(1));
    let line = rope.line(row);
    let character = match encoding {
        Encoding::Utf8 => byte_col as u32,
        Encoding::Utf16 => {
            let mut byte = 0usize;
            let mut units: u32 = 0;
            for ch in line.chars() {
                if byte >= byte_col {
                    break;
                }
                let len = ch.len_utf8();
                if byte + len > byte_col {
                    // Mid-char index: clamp to the boundary we last passed.
                    break;
                }
                byte += len;
                units += ch.len_utf16() as u32;
            }
            units
        }
        Encoding::Utf32 => {
            let mut byte = 0usize;
            let mut units: u32 = 0;
            for ch in line.chars() {
                if byte >= byte_col {
                    break;
                }
                let len = ch.len_utf8();
                if byte + len > byte_col {
                    break;
                }
                byte += len;
                units += 1;
            }
            units
        }
    };
    Position {
        line: row as u32,
        character,
    }
}

/// Convert an LSP `Position` (in the negotiated `encoding`) to a (row, byte_col)
/// coordinate in the rope.
pub fn lsp_to_byte(rope: &Rope, pos: Position, encoding: Encoding) -> (usize, usize) {
    let row = (pos.line as usize).min(rope.len_lines().saturating_sub(1));
    let line = rope.line(row);
    let target = pos.character as usize;
    let mut byte = 0usize;
    match encoding {
        Encoding::Utf8 => {
            byte = target.min(line.len_bytes());
        }
        Encoding::Utf16 => {
            let mut units = 0usize;
            for ch in line.chars() {
                if units >= target {
                    break;
                }
                units += ch.len_utf16();
                byte += ch.len_utf8();
            }
        }
        Encoding::Utf32 => {
            let mut units = 0usize;
            for ch in line.chars() {
                if units >= target {
                    break;
                }
                units += 1;
                byte += ch.len_utf8();
            }
        }
    }
    (row, byte)
}

/// Compute the LSP end position after applying a splice that wrote
/// `inserted` starting at LSP `start`. Helper used when building
/// `TextDocumentContentChangeEvent` ranges where the start of the change
/// is known but the end has to be derived from the splice text alone.
pub fn advance_position(start: Position, text: &str, encoding: Encoding) -> Position {
    let mut line = start.line;
    let mut character = start.character;
    for ch in text.chars() {
        if ch == '\n' {
            line += 1;
            character = 0;
        } else {
            let step = match encoding {
                Encoding::Utf8 => ch.len_utf8() as u32,
                Encoding::Utf16 => ch.len_utf16() as u32,
                Encoding::Utf32 => 1,
            };
            character += step;
        }
    }
    Position { line, character }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::Position;
    use ropey::Rope;

    fn rope(s: &str) -> Rope {
        Rope::from_str(s)
    }

    #[test]
    fn utf8_ascii_roundtrip() {
        let r = rope("hello\nworld");
        let lsp = byte_to_lsp(&r, 1, 3, Encoding::Utf8);
        assert_eq!(lsp, Position { line: 1, character: 3 });
        assert_eq!(lsp_to_byte(&r, lsp, Encoding::Utf8), (1, 3));
    }

    #[test]
    fn utf16_emoji_counts_two_units() {
        // 😀 is U+1F600, 4 UTF-8 bytes, 2 UTF-16 units.
        let r = rope("a😀b");
        let lsp = byte_to_lsp(&r, 0, 5, Encoding::Utf16); // after the emoji
        assert_eq!(lsp, Position { line: 0, character: 3 }); // a(1) + 😀(2)
        assert_eq!(lsp_to_byte(&r, lsp, Encoding::Utf16), (0, 5));
    }

    #[test]
    fn utf32_cjk_counts_one_unit_per_char() {
        let r = rope("日本語");
        let lsp = byte_to_lsp(&r, 0, 6, Encoding::Utf32); // 2 chars in
        assert_eq!(lsp, Position { line: 0, character: 2 });
        assert_eq!(lsp_to_byte(&r, lsp, Encoding::Utf32), (0, 6));
    }

    #[test]
    fn advance_position_handles_newline() {
        let p = advance_position(Position { line: 1, character: 4 }, "x\ny", Encoding::Utf8);
        assert_eq!(p, Position { line: 2, character: 1 });
    }
}
