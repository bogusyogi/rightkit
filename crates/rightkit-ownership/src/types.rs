//! Shared scan value types.

use serde_json::{json, Value};

/// One `unscanned` coverage entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cov {
    pub path: String,
    pub reason: String,
    pub detail: Option<String>,
}

impl Cov {
    pub fn new(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            reason: reason.into(),
            detail: None,
        }
    }

    pub fn with_detail(
        path: impl Into<String>,
        reason: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            reason: reason.into(),
            detail: Some(detail.into()),
        }
    }

    pub fn to_json(&self) -> Value {
        match &self.detail {
            Some(detail) => {
                json!({"path": self.path, "status": "unscanned", "reason": self.reason, "detail": detail})
            }
            None => json!({"path": self.path, "status": "unscanned", "reason": self.reason}),
        }
    }
}

pub fn coverage_json(list: &[Cov]) -> Value {
    Value::Array(list.iter().map(Cov::to_json).collect())
}
