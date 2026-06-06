//! A 2-D point parametric over its coordinate type. The editor uses
//! `Position<usize>` for absolute file positions, `Position<u16>` for
//! viewport-relative cursor positions, and `Position<i16>` for relative
//! cursor deltas (`MoveKind::Relative`).

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct Position<T> {
    pub row: T,
    pub col: T,
}

impl<T> Position<T> {
    pub fn new(col: T, row: T) -> Self {
        Self { row, col }
    }
}

impl<T: Default> Default for Position<T> {
    fn default() -> Self {
        Self {
            row: T::default(),
            col: T::default(),
        }
    }
}
