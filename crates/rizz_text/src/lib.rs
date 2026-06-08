//! Text storage + manipulation for the rizz editor.
//!
//! `Buffer` is the central type: a rope-backed text container that owns its
//! own cursor, viewport, mode, change-tree, and attached metadata (text
//! properties + overlays, soft-wrap settings + cache). All editing operations
//! (`buffer::edits`) and cursor motions (`buffer::cursor`)
//! go through it. Buffers don't know about rendering, key events, or lisp —
//! they're a pure text + cursor abstraction.
//!
//! Supporting modules:
//! - [`motions`]: pure rope motions (`w`/`b`/`e`/`ge` and big-word variants)
//! - [`props`]: text properties + overlays (`PropStore`, `OverlayId`)
//! - [`wrap`]: soft-wrap config + visual-row layout (`WrapMap`)
//! - [`scroll`]: viewport scroll math (pure functions over wrap maps)
//! - [`io`]: read/write a buffer to disk

pub mod buffer;
pub mod io;
pub mod motions;
pub mod props;
pub mod scroll;
pub mod wrap;

pub use buffer::{Buffer, BufferId, MoveKind};
pub use props::{OverlayId, PropEntry, PropStore};
pub use wrap::{VisualRow, WrapConfig, WrapMap, WrapMode, WrapSettings};
