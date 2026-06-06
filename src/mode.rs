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
            _ => return Err("unknown EditingMode"),
        })
    }
}
