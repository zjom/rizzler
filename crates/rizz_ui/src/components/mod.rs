//! Per-widget paint routines — thin glue between a
//! [`crate::render::RenderedBuffer`] (or [`crate::render::StateSnapshot`])
//! and ratatui's primitives.

pub mod editor_view;
pub mod minibuffer;

pub use editor_view::EditorView;
pub use minibuffer::MinibufferLine;
