//! UI for the rizz editor: renderer-agnostic primitives ([`styling`],
//! [`widget`], [`panel`], [`window`], [`render`], [`precompute`]), the
//! concrete ratatui [`render_ratatui`] backend and its [`components`], and
//! the RAII [`terminal`] lifecycle that swaps the host terminal into editor
//! mode.

pub mod components;
pub mod panel;
pub mod precompute;
pub mod render;
pub mod render_ratatui;
pub mod styling;
pub mod terminal;
pub mod widget;
pub mod window;
pub use rizz_text::scroll;

pub use render::{
    CursorStyle, DecoratorRanges, RenderedBuffer, RenderedFrame, RenderedGutter, Renderer,
    StateSnapshot, StyledRange,
};
pub use render_ratatui::RatatuiRenderer;
pub use styling::{Color, Style, Theme, ThemeCell};
pub use terminal::{TerminalGuard, install_panic_hook};
pub use widget::Widget;
pub use window::{LeafLayout, Window, WindowTree};
