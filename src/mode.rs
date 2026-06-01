#![allow(dead_code)]

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
