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
