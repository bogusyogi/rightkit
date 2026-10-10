use crate::error::Result;

/// Redacted secret string, shared with the other net crates (`rightkit_http::Secret`).
pub use rightkit_http::Secret;

/// OAuth / bearer hook implemented by the host app. It owns the PKCE dance and the keychain;
/// this crate only asks for a token per request and reports 401s.
pub trait AuthProvider: Send + Sync {
    /// Current bearer token for `server`, if any.
    fn bearer(&self, server: &str) -> Result<Option<Secret>>;
    /// Called once per request on HTTP 401. Return Ok(true) after refreshing so the request is retried once.
    fn on_unauthorized(&self, server: &str, www_authenticate: Option<&str>) -> Result<bool>;
}
