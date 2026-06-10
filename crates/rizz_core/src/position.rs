//! A 2-D point whose coordinate *space* is encoded in its type rather than
//! left implicit in the integer width. Each [`Space`] marker fixes both the
//! semantic meaning of the position and the integer type of its coordinates,
//! so the three flavors the editor uses can never be confused at a call site:
//!
//! - [`FilePos`] — absolute position in the file's character grid (`usize`).
//! - [`ScreenPos`] — viewport-relative position in terminal cells (`u16`).
//! - [`PosDelta`] — a signed cursor delta, e.g. `MoveKind::Relative` (`i16`).
//!
//! The markers are uninhabited: they exist only as type-level tags and are
//! never constructed.

use std::fmt;
use std::hash::{Hash, Hasher};

/// Coordinate space a [`Position`] lives in. The implementing marker type
/// fixes the integer type ([`Space::Unit`]) of the position's coordinates.
pub trait Space {
    /// Integer type of the `row`/`col` coordinates in this space.
    type Unit;
}

/// Marker for absolute positions in the file's character grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum File {}

/// Marker for viewport-relative positions in terminal cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Screen {}

/// Marker for signed cursor deltas (relative motions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Delta {}

impl Space for File {
    type Unit = usize;
}

impl Space for Screen {
    type Unit = u16;
}

impl Space for Delta {
    type Unit = i16;
}

pub struct Position<S: Space> {
    pub row: S::Unit,
    pub col: S::Unit,
}

/// Absolute position in the file's character grid (0-based `usize` row/col).
pub type FilePos = Position<File>;
/// Viewport-relative position in terminal cells (`u16` row/col).
pub type ScreenPos = Position<Screen>;
/// Signed cursor delta, e.g. `MoveKind::Relative` (`i16` row/col).
pub type PosDelta = Position<Delta>;

impl<S: Space> Position<S> {
    pub fn new(col: S::Unit, row: S::Unit) -> Self {
        Self { row, col }
    }
}

// The derive macros would bound the marker `S` (e.g. `S: Clone`) instead of
// the coordinate type `S::Unit`, which is what actually needs the bound, so
// these traits are implemented by hand.

impl<S: Space> Clone for Position<S>
where
    S::Unit: Clone,
{
    fn clone(&self) -> Self {
        Self {
            row: self.row.clone(),
            col: self.col.clone(),
        }
    }
}

impl<S: Space> Copy for Position<S> where S::Unit: Copy {}

impl<S: Space> fmt::Debug for Position<S>
where
    S::Unit: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Position")
            .field("row", &self.row)
            .field("col", &self.col)
            .finish()
    }
}

impl<S: Space> PartialEq for Position<S>
where
    S::Unit: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.row == other.row && self.col == other.col
    }
}

impl<S: Space> Eq for Position<S> where S::Unit: Eq {}

impl<S: Space> Hash for Position<S>
where
    S::Unit: Hash,
{
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.row.hash(state);
        self.col.hash(state);
    }
}

impl<S: Space> Default for Position<S>
where
    S::Unit: Default,
{
    fn default() -> Self {
        Self {
            row: S::Unit::default(),
            col: S::Unit::default(),
        }
    }
}
