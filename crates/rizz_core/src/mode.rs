//! The editor's modal-editing state.
//!
//! `EditingMode` is the single source of truth for which mode a buffer is in.
//! Used by the keymap to look up which bindings apply, by the renderer to
//! pick a cursor shape, and by the buffer to gate column clamping (Normal
//! mode keeps the cursor on a character; the others may sit past).

use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EditingMode {
    Insert,
    #[default]
    Normal,
    Visual,
    VisualLine,
    VisualBlock,
    Command,
    /// Vim `R` — typed characters overwrite the char under the cursor and
    /// advance. At end-of-line the typed char extends the line.
    Replace,
}

impl EditingMode {
    pub fn is_visual(self) -> bool {
        matches!(self, Self::Visual | Self::VisualLine | Self::VisualBlock)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Insert => "insert",
            Self::Visual => "visual",
            Self::VisualLine => "visual-line",
            Self::VisualBlock => "visual-block",
            Self::Command => "command",
            Self::Replace => "replace",
        }
    }

    /// Single-character glyph used by the status-line mode indicator.
    pub fn as_glyph(&self) -> &'static str {
        match self {
            Self::Insert => "i",
            Self::Normal => "n",
            Self::Visual => "v",
            Self::VisualLine => "V",
            Self::VisualBlock => "^V",
            Self::Command => "c",
            Self::Replace => "R",
        }
    }
}

impl FromStr for EditingMode {
    type Err = &'static str;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "normal" => EditingMode::Normal,
            "insert" => EditingMode::Insert,
            "visual" => EditingMode::Visual,
            "visual-line" => EditingMode::VisualLine,
            "visual-block" => EditingMode::VisualBlock,
            "command" => EditingMode::Command,
            "replace" => EditingMode::Replace,
            _ => return Err("unknown EditingMode"),
        })
    }
}
