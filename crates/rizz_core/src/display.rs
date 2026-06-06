//! Visual substitution for a styled range.
//!
//! Carried by both [`crate::PropEntry`]-style records (in `rizz_text`) and
//! by the renderer's `StyledRange` (in `rizz_ui`). Shared here because both
//! crates need the same shape and neither should be the canonical owner.

use std::rc::Rc;

#[derive(Clone, Debug)]
pub enum Display {
    /// Replace the range with `s`. Width comes from the string itself.
    String(Rc<str>),
    /// Replace the range with `n` blank cells.
    Space(usize),
}
