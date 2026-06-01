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
