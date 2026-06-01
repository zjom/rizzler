use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};

use crate::render::StateSnapshot;

pub mod editor_view;
pub mod minibuffer;
pub mod status_line;

pub use editor_view::EditorView;
pub use minibuffer::MinibufferLine;
pub use status_line::StatusLine;

/// A region of the editor frame that renders itself given a snapshot.
///
/// The renderer stacks components vertically, asking each for its vertical
/// constraint and a renderer for its assigned rect. Components that own the
/// cursor return its screen position from [`Component::cursor`]; the renderer
/// uses the last `Some` it sees.
pub trait Component {
    fn constraint(&self) -> Constraint;
    fn render(&self, area: Rect, snap: &StateSnapshot<'_>, frame: &mut Frame);
    fn cursor(&self, _area: Rect, _snap: &StateSnapshot<'_>) -> Option<(u16, u16)> {
        None
    }
}
