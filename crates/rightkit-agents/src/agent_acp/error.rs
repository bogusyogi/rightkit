//! Standard JSON-RPC 2.0 + ACP-specific error codes. Mirrors
//! `agent-client-protocol-schema/src/v1/error.rs::ErrorCode` exactly (the
//! same integer codes), so a real ACP client's error handling matches.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpErrorCode {
    ParseError,
    InvalidRequest,
    MethodNotFound,
    InvalidParams,
    InternalError,
    RequestCancelled,
    AuthRequired,
    ResourceNotFound,
}

impl AcpErrorCode {
    pub const fn code(self) -> i64 {
        match self {
            AcpErrorCode::ParseError => -32700,
            AcpErrorCode::InvalidRequest => -32600,
            AcpErrorCode::MethodNotFound => -32601,
            AcpErrorCode::InvalidParams => -32602,
            AcpErrorCode::InternalError => -32603,
            AcpErrorCode::RequestCancelled => -32800,
            AcpErrorCode::AuthRequired => -32000,
            AcpErrorCode::ResourceNotFound => -32002,
        }
    }

    pub const fn message(self) -> &'static str {
        match self {
            AcpErrorCode::ParseError => "Parse error",
            AcpErrorCode::InvalidRequest => "Invalid request",
            AcpErrorCode::MethodNotFound => "Method not found",
            AcpErrorCode::InvalidParams => "Invalid params",
            AcpErrorCode::InternalError => "Internal error",
            AcpErrorCode::RequestCancelled => "Request cancelled",
            AcpErrorCode::AuthRequired => "Authentication required",
            AcpErrorCode::ResourceNotFound => "Resource not found",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AcpError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl AcpError {
    pub fn new(code: AcpErrorCode) -> Self {
        Self {
            code: code.code(),
            message: code.message().to_string(),
            data: None,
        }
    }

    pub fn with_message(code: AcpErrorCode, message: impl Into<String>) -> Self {
        Self {
            code: code.code(),
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn parse_error(detail: impl Into<String>) -> Self {
        Self::with_message(AcpErrorCode::ParseError, detail)
    }

    pub fn invalid_request() -> Self {
        Self::new(AcpErrorCode::InvalidRequest)
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::with_message(
            AcpErrorCode::MethodNotFound,
            format!("method not found: {method}"),
        )
    }

    pub fn invalid_params(detail: impl Into<String>) -> Self {
        Self::with_message(AcpErrorCode::InvalidParams, detail)
    }

    pub fn internal_error(detail: impl Into<String>) -> Self {
        Self::with_message(AcpErrorCode::InternalError, detail)
    }

    pub fn resource_not_found(uri: Option<&str>) -> Self {
        let error = Self::new(AcpErrorCode::ResourceNotFound);
        match uri {
            Some(uri) => error.with_data(serde_json::json!({ "uri": uri })),
            None => error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_match_spec_integers() {
        assert_eq!(AcpErrorCode::ParseError.code(), -32700);
        assert_eq!(AcpErrorCode::InvalidRequest.code(), -32600);
        assert_eq!(AcpErrorCode::MethodNotFound.code(), -32601);
        assert_eq!(AcpErrorCode::InvalidParams.code(), -32602);
        assert_eq!(AcpErrorCode::InternalError.code(), -32603);
        assert_eq!(AcpErrorCode::RequestCancelled.code(), -32800);
        assert_eq!(AcpErrorCode::AuthRequired.code(), -32000);
        assert_eq!(AcpErrorCode::ResourceNotFound.code(), -32002);
    }

    #[test]
    fn method_not_found_includes_method_name() {
        let error = AcpError::method_not_found("acp/bogus");
        assert!(error.message.contains("acp/bogus"));
        assert_eq!(error.code, -32601);
    }

    #[test]
    fn resource_not_found_attaches_uri_data() {
        let error = AcpError::resource_not_found(Some("file:///missing.txt"));
        assert_eq!(error.data.unwrap()["uri"], "file:///missing.txt");
    }
}
