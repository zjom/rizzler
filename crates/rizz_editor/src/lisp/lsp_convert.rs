//! Convert owned LSP snapshot types ([`CompletionItemOwned`],
//! [`CodeActionOwned`], …) into structured rizz [`Value`]s for the
//! `set-lsp-completion-fn` / `set-lsp-code-action-fn` callbacks.

use std::rc::Rc;
use std::sync::Arc;

use im::HashMap;
use rizz::runtime::Value;

use rizz_actions::{CodeActionOwned, CompletionItemKindOwned, CompletionItemOwned};
use rizz_core::Position;

fn key(s: &str) -> Rc<Value> {
    Rc::new(Value::Str(s.into()))
}

fn arc_to_rc(s: &Arc<str>) -> Rc<str> {
    Rc::from(s.as_ref())
}

fn opt_str(s: Option<&Arc<str>>) -> Rc<Value> {
    match s {
        Some(s) => Rc::new(Value::Str(arc_to_rc(s))),
        None => Rc::new(Value::Unit),
    }
}

fn completion_kind_to_ident(k: CompletionItemKindOwned) -> Rc<Value> {
    let s: &'static str = match k {
        CompletionItemKindOwned::Text => "text",
        CompletionItemKindOwned::Method => "method",
        CompletionItemKindOwned::Function => "function",
        CompletionItemKindOwned::Constructor => "constructor",
        CompletionItemKindOwned::Field => "field",
        CompletionItemKindOwned::Variable => "variable",
        CompletionItemKindOwned::Class => "class",
        CompletionItemKindOwned::Interface => "interface",
        CompletionItemKindOwned::Module => "module",
        CompletionItemKindOwned::Property => "property",
        CompletionItemKindOwned::Enum => "enum",
        CompletionItemKindOwned::Keyword => "keyword",
        CompletionItemKindOwned::Snippet => "snippet",
        CompletionItemKindOwned::Other => "other",
    };
    Rc::new(Value::Ident(s.into()))
}

pub fn position_to_value(p: Position<usize>) -> Rc<Value> {
    let mut m: HashMap<Rc<Value>, Rc<Value>> = HashMap::new();
    m.insert(key("row"), Rc::new(Value::Int(p.row as i64)));
    m.insert(key("col"), Rc::new(Value::Int(p.col as i64)));
    Rc::new(Value::Map(m))
}

pub fn completion_item_to_value(id: usize, item: &CompletionItemOwned) -> Rc<Value> {
    let mut m: HashMap<Rc<Value>, Rc<Value>> = HashMap::new();
    m.insert(key("id"), Rc::new(Value::Int(id as i64)));
    m.insert(key("label"), Rc::new(Value::Str(arc_to_rc(&item.label))));
    m.insert(key("detail"), opt_str(item.detail.as_ref()));
    m.insert(
        key("insert-text"),
        Rc::new(Value::Str(arc_to_rc(&item.insert_text))),
    );
    m.insert(key("kind"), completion_kind_to_ident(item.kind));
    Rc::new(Value::Map(m))
}

pub fn completion_items_to_value(items: &[CompletionItemOwned]) -> Rc<Value> {
    let arr: im::Vector<Rc<Value>> = items
        .iter()
        .enumerate()
        .map(|(i, it)| completion_item_to_value(i, it))
        .collect();
    Rc::new(Value::Array(arr))
}

pub fn code_action_to_value(id: usize, action: &CodeActionOwned) -> Rc<Value> {
    let mut m: HashMap<Rc<Value>, Rc<Value>> = HashMap::new();
    m.insert(key("id"), Rc::new(Value::Int(id as i64)));
    m.insert(key("title"), Rc::new(Value::Str(arc_to_rc(&action.title))));
    m.insert(key("kind"), opt_str(action.kind.as_ref()));
    m.insert(
        key("has-edit"),
        Rc::new(Value::Int(action.edit.is_some() as i64)),
    );
    m.insert(
        key("has-command"),
        Rc::new(Value::Int(action.command.is_some() as i64)),
    );
    Rc::new(Value::Map(m))
}

pub fn code_actions_to_value(actions: &[CodeActionOwned]) -> Rc<Value> {
    let arr: im::Vector<Rc<Value>> = actions
        .iter()
        .enumerate()
        .map(|(i, a)| code_action_to_value(i, a))
        .collect();
    Rc::new(Value::Array(arr))
}
