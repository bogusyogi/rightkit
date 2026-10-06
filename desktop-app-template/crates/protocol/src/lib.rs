//! Shell <-> engine wire contract. Product schemas live here; transport is `framed`.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod framed;

/// Per-frame ceiling (bytes, excluding newline). Large payloads go through files, not frames.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Request {
    pub id: u64,
    pub cmd: String,
    #[serde(default)]
    pub input: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Response {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

impl Response {
    pub fn ok(id: u64, result: Value) -> Self {
        Self { id, result: Some(result), error: None }
    }
    pub fn err(id: u64, code: &str, message: impl Into<String>) -> Self {
        Self { id, result: None, error: Some(ErrorBody { code: code.into(), message: message.into() }) }
    }
}
