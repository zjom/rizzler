//! Owned snapshot types for LSP responses carried by `Action` variants.
//!
//! Keeping these in `rizz_actions` instead of pulling in `lsp-types` lets
//! the closed [`Action`] enum stay `Clone + Eq + Hash` without leaking
//! tokio/serde into the action layer. `rizz_lsp::action_bridge` converts
//! `lsp_types::*` values into these shapes at the boundary, and the
//! editor's `apply` arms consume them directly.

use std::sync::Arc;

use rizz_core::Position;

/// Stable identifier for a spawned language-server client. Lets the editor
/// side refer to clients by value without depending on `rizz_lsp`.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct LspClientId(pub u64);

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct RangeOwned {
    pub start: Position<usize>,
    pub end: Position<usize>,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct LocationOwned {
    /// Target file URI (`file://...`). The receiver converts it into an
    /// absolute path before opening.
    pub uri: Arc<str>,
    pub range: RangeOwned,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct TextEditOwned {
    pub range: RangeOwned,
    pub new_text: Arc<str>,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct DocumentEditOwned {
    pub uri: Arc<str>,
    /// LSP-server document version this edit was issued against. `None`
    /// means "any version". The editor side validates before applying.
    pub version: Option<i32>,
    pub edits: Arc<[TextEditOwned]>,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct WorkspaceEditOwned {
    pub changes: Arc<[DocumentEditOwned]>,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct CompletionItemOwned {
    pub label: Arc<str>,
    pub detail: Option<Arc<str>>,
    /// Plain text to insert at the cursor. Snippet expansion is out of
    /// scope for MVP — snippets are inserted as their literal body.
    pub insert_text: Arc<str>,
    pub kind: CompletionItemKindOwned,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum CompletionItemKindOwned {
    Text,
    Method,
    Function,
    Constructor,
    Field,
    Variable,
    Class,
    Interface,
    Module,
    Property,
    Enum,
    Keyword,
    Snippet,
    Other,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct CodeActionOwned {
    pub title: Arc<str>,
    pub kind: Option<Arc<str>>,
    /// Either a workspace edit to apply directly, a server command to
    /// invoke via `workspace/executeCommand`, or both.
    pub edit: Option<WorkspaceEditOwned>,
    pub command: Option<CommandOwned>,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct CommandOwned {
    pub title: Arc<str>,
    pub command: Arc<str>,
    /// Argument list serialized as JSON strings. The bridge re-encodes
    /// the original `serde_json::Value` arguments at apply time so we
    /// don't have to round-trip through more owned shapes.
    pub arguments_json: Arc<[Arc<str>]>,
}
