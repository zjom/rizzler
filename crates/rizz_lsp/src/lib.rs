//! LSP client runtime for the rizz editor.
//!
//! The editor's main loop is synchronous; tokio lives behind this crate
//! on a dedicated background thread. Communication with the editor uses
//! `crossbeam_channel`:
//!
//! 1. `LspRegistry::ensure_running` spawns (or reuses) a `LspClient`
//!    tokio task. The handshake happens inside the runtime; the editor
//!    blocks briefly on a bounded reply channel until `initialize` clears.
//! 2. Per-buffer state lives in [`LspBufferAttachment`], a concrete impl
//!    of the type-erased `rizz_text::LspBufferHandle` trait. The buffer's
//!    edit sites forward rope splices into the attachment, which queues
//!    `textDocument/didChange` notifications on the runtime channel; the
//!    client task debounces and ships them.
//! 3. Server responses and notifications flow back through
//!    `runtime().events_rx`. The editor drains the receiver each main-loop
//!    tick and synthesizes `Action::Lsp*` variants from each event.
//!
//! The runtime is process-wide and lazily started on first use.

pub mod action_bridge;
mod client;
mod codec;
mod document;
mod error;
mod event;
mod message;
mod position;
mod registry;
mod runtime;

pub use document::LspBufferAttachment;
pub use error::LspError;
pub use event::{ChangeEvent, LspEvent, RequestSeq, RuntimeCmd, SpawnReply};
pub use position::{advance_position, byte_to_lsp, lsp_to_byte, Encoding};
pub use registry::{LspRegistry, RunningClient};
pub use runtime::{runtime, LspRuntime};
