//! Convert `lsp-types` response values into the owned snapshot types that
//! `rizz_actions::Action` carries, so the action layer stays free of
//! `lsp-types`.

use std::sync::Arc;

use lsp_types::{
    CodeActionOrCommand, CompletionItem, CompletionItemKind, CompletionResponse, Diagnostic,
    DocumentChangeOperation, DocumentChanges, GotoDefinitionResponse, Location, OneOf,
    TextDocumentEdit, TextEdit, Uri, WorkspaceEdit,
};
use ropey::Rope;

use rizz_actions::{
    CodeActionOwned, CommandOwned, CompletionItemKindOwned, CompletionItemOwned,
    DocumentEditOwned, LocationOwned, RangeOwned, TextEditOwned, WorkspaceEditOwned,
};
use rizz_core::{LspDiagnostic, Position, Severity};

use crate::position::{lsp_to_byte, Encoding};

fn pos_owned(rope: &Rope, p: lsp_types::Position, enc: Encoding) -> Position<usize> {
    let (row, col) = lsp_to_byte(rope, p, enc);
    Position::new(col, row)
}

fn range_owned(rope: &Rope, r: lsp_types::Range, enc: Encoding) -> RangeOwned {
    RangeOwned {
        start: pos_owned(rope, r.start, enc),
        end: pos_owned(rope, r.end, enc),
    }
}

pub fn diagnostic_owned(rope: &Rope, d: &Diagnostic, enc: Encoding) -> LspDiagnostic {
    let severity = match d.severity {
        Some(lsp_types::DiagnosticSeverity::ERROR) => Severity::Error,
        Some(lsp_types::DiagnosticSeverity::WARNING) => Severity::Warning,
        Some(lsp_types::DiagnosticSeverity::INFORMATION) => Severity::Info,
        Some(lsp_types::DiagnosticSeverity::HINT) => Severity::Hint,
        _ => Severity::Error,
    };
    let code = d.code.as_ref().map(|c| -> Arc<str> {
        match c {
            lsp_types::NumberOrString::Number(n) => Arc::from(n.to_string()),
            lsp_types::NumberOrString::String(s) => Arc::from(s.as_str()),
        }
    });
    let start = pos_owned(rope, d.range.start, enc);
    let end = pos_owned(rope, d.range.end, enc);
    LspDiagnostic {
        start,
        end,
        severity,
        message: Arc::from(d.message.as_str()),
        source: d.source.as_deref().map(Arc::from),
        code,
    }
}

pub fn location_owned(_rope: &Rope, loc: &Location, _enc: Encoding) -> LocationOwned {
    // The range references the *target* document, not our rope. Store raw
    // LSP line/character and let the editor re-convert when it opens the
    // target file with the right rope.
    LocationOwned {
        uri: Arc::from(url_str(&loc.uri)),
        range: RangeOwned {
            start: Position::new(loc.range.start.character as usize, loc.range.start.line as usize),
            end: Position::new(loc.range.end.character as usize, loc.range.end.line as usize),
        },
    }
}

pub fn locations_owned(rope: &Rope, resp: GotoDefinitionResponse, enc: Encoding) -> Vec<LocationOwned> {
    match resp {
        GotoDefinitionResponse::Scalar(l) => vec![location_owned(rope, &l, enc)],
        GotoDefinitionResponse::Array(items) => {
            items.iter().map(|l| location_owned(rope, l, enc)).collect()
        }
        GotoDefinitionResponse::Link(items) => items
            .into_iter()
            .map(|link| LocationOwned {
                uri: Arc::from(url_str(&link.target_uri)),
                range: RangeOwned {
                    start: Position::new(
                        link.target_selection_range.start.character as usize,
                        link.target_selection_range.start.line as usize,
                    ),
                    end: Position::new(
                        link.target_selection_range.end.character as usize,
                        link.target_selection_range.end.line as usize,
                    ),
                },
            })
            .collect(),
    }
}

pub fn completion_items(resp: CompletionResponse) -> Vec<CompletionItemOwned> {
    let items: Vec<CompletionItem> = match resp {
        CompletionResponse::Array(items) => items,
        CompletionResponse::List(list) => list.items,
    };
    items.into_iter().map(completion_item_owned).collect()
}

fn completion_item_owned(item: CompletionItem) -> CompletionItemOwned {
    let insert_text = item
        .insert_text
        .clone()
        .or_else(|| Some(item.label.clone()))
        .unwrap_or_default();
    let kind = match item.kind {
        Some(CompletionItemKind::TEXT) => CompletionItemKindOwned::Text,
        Some(CompletionItemKind::METHOD) => CompletionItemKindOwned::Method,
        Some(CompletionItemKind::FUNCTION) => CompletionItemKindOwned::Function,
        Some(CompletionItemKind::CONSTRUCTOR) => CompletionItemKindOwned::Constructor,
        Some(CompletionItemKind::FIELD) => CompletionItemKindOwned::Field,
        Some(CompletionItemKind::VARIABLE) => CompletionItemKindOwned::Variable,
        Some(CompletionItemKind::CLASS) => CompletionItemKindOwned::Class,
        Some(CompletionItemKind::INTERFACE) => CompletionItemKindOwned::Interface,
        Some(CompletionItemKind::MODULE) => CompletionItemKindOwned::Module,
        Some(CompletionItemKind::PROPERTY) => CompletionItemKindOwned::Property,
        Some(CompletionItemKind::ENUM) => CompletionItemKindOwned::Enum,
        Some(CompletionItemKind::KEYWORD) => CompletionItemKindOwned::Keyword,
        Some(CompletionItemKind::SNIPPET) => CompletionItemKindOwned::Snippet,
        _ => CompletionItemKindOwned::Other,
    };
    CompletionItemOwned {
        label: Arc::from(item.label.as_str()),
        detail: item.detail.as_deref().map(Arc::from),
        insert_text: Arc::from(insert_text.as_str()),
        kind,
    }
}

pub fn text_edits_owned(edits: Vec<TextEdit>) -> Vec<TextEditOwned> {
    edits
        .into_iter()
        .map(|e| TextEditOwned {
            range: RangeOwned {
                start: Position::new(
                    e.range.start.character as usize,
                    e.range.start.line as usize,
                ),
                end: Position::new(e.range.end.character as usize, e.range.end.line as usize),
            },
            new_text: Arc::from(e.new_text.as_str()),
        })
        .collect()
}

pub fn workspace_edit_owned(edit: WorkspaceEdit) -> WorkspaceEditOwned {
    let mut docs: Vec<DocumentEditOwned> = Vec::new();
    if let Some(changes) = edit.changes {
        for (uri, edits) in changes {
            docs.push(DocumentEditOwned {
                uri: Arc::from(url_str(&uri)),
                version: None,
                edits: Arc::from(text_edits_owned(edits)),
            });
        }
    }
    if let Some(document_changes) = edit.document_changes {
        match document_changes {
            DocumentChanges::Edits(edits) => {
                for e in edits {
                    docs.push(text_document_edit_owned(e));
                }
            }
            DocumentChanges::Operations(ops) => {
                for op in ops {
                    if let DocumentChangeOperation::Edit(e) = op {
                        docs.push(text_document_edit_owned(e));
                    }
                    // Create/Rename/Delete file operations are out of scope.
                }
            }
        }
    }
    WorkspaceEditOwned {
        changes: Arc::from(docs),
    }
}

fn text_document_edit_owned(e: TextDocumentEdit) -> DocumentEditOwned {
    let edits: Vec<TextEdit> = e
        .edits
        .into_iter()
        .map(|annotated| match annotated {
            OneOf::Left(te) => te,
            OneOf::Right(annot) => annot.text_edit,
        })
        .collect();
    DocumentEditOwned {
        uri: Arc::from(url_str(&e.text_document.uri)),
        version: e.text_document.version,
        edits: Arc::from(text_edits_owned(edits)),
    }
}

pub fn code_actions_owned(
    items: Vec<CodeActionOrCommand>,
) -> Vec<CodeActionOwned> {
    items
        .into_iter()
        .map(|item| match item {
            CodeActionOrCommand::Command(cmd) => CodeActionOwned {
                title: Arc::from(cmd.title.as_str()),
                kind: None,
                edit: None,
                command: Some(command_owned(cmd)),
            },
            CodeActionOrCommand::CodeAction(action) => CodeActionOwned {
                title: Arc::from(action.title.as_str()),
                kind: action.kind.as_ref().map(|k| Arc::from(k.as_str())),
                edit: action.edit.map(workspace_edit_owned),
                command: action.command.map(command_owned),
            },
        })
        .collect()
}

fn command_owned(cmd: lsp_types::Command) -> CommandOwned {
    let args: Vec<Arc<str>> = cmd
        .arguments
        .unwrap_or_default()
        .into_iter()
        .map(|v| -> Arc<str> { Arc::from(v.to_string()) })
        .collect();
    CommandOwned {
        title: Arc::from(cmd.title.as_str()),
        command: Arc::from(cmd.command.as_str()),
        arguments_json: Arc::from(args),
    }
}

fn url_str(uri: &Uri) -> String {
    uri.to_string()
}
