//! UI for the rizz editor. Two halves:
//!
//! - **Renderer-agnostic primitives** — [`styling`] (Style/Color/Theme),
//!   [`widget`] (declarative widget tree built from lisp), [`popup`]
//!   (overlay placement + stack), [`window`] (window tree), [`render`]
//!   (Renderer trait + RenderedFrame data), [`scroll_math`] (re-export from
//!   rizz_text), [`precompute`] (frame assembly: gutter rows, decorator
//!   ranges, soft-wrap maps).
//! - **Concrete ratatui renderer + components** — [`render_ratatui`] walks
//!   a `RenderedFrame` into ratatui draws; [`components`] (`EditorView`,
//!   `MinibufferLine`) are the per-widget paint routines.
//! - **Terminal lifecycle** — [`terminal::TerminalGuard`] installs raw mode +
//!   alt screen and restores them on drop (or panic).

pub mod components;
pub mod popup;
pub mod precompute;
pub mod render;
pub mod render_ratatui;
pub mod styling;
pub mod terminal;
pub mod widget;
pub mod window;

pub use render::{
    CursorStyle, DecoratorRanges, RenderedBuffer, RenderedFrame, RenderedGutter, Renderer,
    StateSnapshot, StyledRange,
};
pub use render_ratatui::RatatuiRenderer;
pub use styling::{Color, Style, Theme, ThemeCell};
pub use terminal::{TerminalGuard, install_panic_hook};
pub use widget::Widget;
pub use window::{LeafLayout, Window, WindowTree};
