//! Soft-wrap configuration shared between `Buffer` (where it's stored) and
//! `components::wrap::WrapMap` (where it's applied). Pulled to its own
//! top-level module so neither side has to reach across the other.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WrapMode {
    /// No wrapping. `WrapMap` is not built; render walks rope lines directly.
    #[default]
    None,
    /// Break at the column limit, mid-word if necessary.
    Char,
    /// Break at the last whitespace before the column limit. Falls back to
    /// `Char` for tokens longer than the wrap width.
    Word,
}

impl WrapMode {
    pub fn as_str(self) -> &'static str {
        match self {
            WrapMode::None => "none",
            WrapMode::Char => "char",
            WrapMode::Word => "word",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "none" | "off" => WrapMode::None,
            "char" => WrapMode::Char,
            "word" => WrapMode::Word,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct WrapConfig {
    pub mode: WrapMode,
    /// Wrap width in cells. The caller passes the content area width
    /// (post-gutter) — `WrapMap` doesn't know about gutters.
    pub width: u16,
    /// If true, continuation rows are prefixed with leading whitespace
    /// matching the original line's indent.
    pub breakindent: bool,
}
