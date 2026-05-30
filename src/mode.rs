#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditingMode {
    Insert,
    Normal,
    Visual,
    Command,
}
