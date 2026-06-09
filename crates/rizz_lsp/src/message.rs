//! Minimal JSON-RPC envelope types for LSP traffic.
//!
//! `lsp-types` defines request/notification *parameter* shapes but not the
//! transport envelope. We define just enough to dispatch on `id` vs
//! `method` so the receive loop can route responses to their pending
//! callbacks and notifications to the editor's event channel.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(i64),
    String(String),
}

impl RequestId {
    pub const fn number(n: i64) -> Self {
        RequestId::Number(n)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct OutgoingRequest {
    pub jsonrpc: &'static str,
    pub id: RequestId,
    pub method: &'static str,
    pub params: Value,
}

impl OutgoingRequest {
    pub fn new(id: RequestId, method: &'static str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            method,
            params,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct OutgoingNotification {
    pub jsonrpc: &'static str,
    pub method: &'static str,
    pub params: Value,
}

impl OutgoingNotification {
    pub fn new(method: &'static str, params: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            method,
            params,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct OutgoingResponse {
    pub jsonrpc: &'static str,
    pub id: RequestId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseError {
    pub code: i64,
    pub message: String,
}

/// One inbound LSP message. JSON-RPC allows three shapes — request,
/// notification, or response — distinguished by the presence of `method`
/// and `id`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum IncomingMessage {
    Response {
        #[serde(default)]
        jsonrpc: Option<String>,
        id: RequestId,
        #[serde(default)]
        result: Option<Value>,
        #[serde(default)]
        error: Option<ResponseError>,
    },
    Request {
        #[serde(default)]
        jsonrpc: Option<String>,
        id: RequestId,
        method: String,
        #[serde(default)]
        params: Value,
    },
    Notification {
        #[serde(default)]
        jsonrpc: Option<String>,
        method: String,
        #[serde(default)]
        params: Value,
    },
}
