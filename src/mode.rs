#![allow(dead_code)]

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EditingMode {
    Insert,
    Normal,
    Visual,
    Command,
}
