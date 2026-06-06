//! Per-widget paint routines.
//!
//! Components live one level down from the top-level renderer so each one
//! stays a thin glue layer between a [`crate::render::RenderedBuffer`] (or
//! [`crate::render::StateSnapshot`]) and ratatui's primitives.

pub mod editor_view;
pub mod minibuffer;

pub use editor_view::EditorView;
pub use minibuffer::MinibufferLine;
