#![allow(dead_code)]

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EditingMode {
    Insert,
    Normal,
    Visual,
    Command,
}

impl Default for EditingMode {
    fn default() -> Self {
        Self::Normal
    }
}
