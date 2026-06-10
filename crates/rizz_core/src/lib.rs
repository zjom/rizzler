//! Foundational pure types shared across the editor crates.
//!
//! Every type in this crate is a value type with no editor-side state and no
//! UI-framework dependencies (ropey is the only allowed third-party import,
//! since selection slicing operates over ropes). Anything higher-level —
//! buffers, themes, renderers — lives in `rizz_text` or above.

pub mod diagnostic;
pub mod display;
pub mod mode;
pub mod position;
pub mod selection;
pub mod window_dir;

pub use diagnostic::{LspDiagnostic, Severity};
pub use display::Display;
pub use mode::EditingMode;
pub use position::{Delta, File, FilePos, PosDelta, Position, Screen, ScreenPos, Space};
pub use window_dir::{FocusDir, SplitDir};
