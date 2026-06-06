//! Pure direction enums for the window tree.
//!
//! `WindowTree` itself lives in `rizz_ui` (it depends on ratatui's `Rect`
//! for layout), but these enums are needed by `rizz_actions::Action` —
//! pulling them down to the core crate avoids a `rizz_actions -> rizz_ui`
//! upward dependency.

/// Orientation of a window split. `Horizontal` lays children side-by-side
/// (vim's `:vsplit`); `Vertical` stacks them top-to-bottom (vim's `:split`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SplitDir {
    Horizontal,
    Vertical,
}

/// Cardinal direction for moving window focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FocusDir {
    Left,
    Right,
    Up,
    Down,
}
