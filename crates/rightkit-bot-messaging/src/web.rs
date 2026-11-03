//! Web UI bearer-token authentication.
//!
//! The web adapter serves the bundled SPA and proxies the same SSE+POST API as Tauri.
//! Auth is a bearer token (the daemon's startup token), accepted either as an
//! `Authorization: Bearer <token>` header or a `?token=<token>` query param.

/// The outcome of a web auth check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebAuthDecision {
    Authorized,
    Rejected,
}

impl WebAuthDecision {
    pub fn is_authorized(&self) -> bool {
        matches!(self, WebAuthDecision::Authorized)
    }
}

/// Validates incoming web requests against the daemon token.
#[derive(Debug, Clone)]
pub struct WebAuth {
    token: String,
}

impl WebAuth {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
        }
    }

    /// Check an `Authorization` header value, e.g. `"Bearer abc123"`.
    pub fn check_header(&self, header_value: &str) -> WebAuthDecision {
        match header_value.strip_prefix("Bearer ") {
            Some(t) if self.constant_eq(t.trim()) => WebAuthDecision::Authorized,
            _ => WebAuthDecision::Rejected,
        }
    }

    /// Check a `?token=` query parameter value.
    pub fn check_query(&self, token: &str) -> WebAuthDecision {
        if self.constant_eq(token) {
            WebAuthDecision::Authorized
        } else {
            WebAuthDecision::Rejected
        }
    }

    /// Resolve auth from either source; header takes priority if present.
    pub fn check(&self, header: Option<&str>, query_token: Option<&str>) -> WebAuthDecision {
        if let Some(h) = header {
            let d = self.check_header(h);
            if d.is_authorized() {
                return d;
            }
        }
        if let Some(q) = query_token {
            return self.check_query(q);
        }
        WebAuthDecision::Rejected
    }

    /// Constant-time comparison (`rightkit-crypto`); unequal lengths are not equal.
    fn constant_eq(&self, candidate: &str) -> bool {
        rightkit_crypto::ct_eq(self.token.as_bytes(), candidate.as_bytes())
    }
}
