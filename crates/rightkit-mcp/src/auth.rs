use std::fmt;

use crate::error::Result;

/// A secret string whose `Debug` never reveals the value. The crate never stores or logs it.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Secret(s.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// OAuth / bearer hook implemented by the host app. It owns the PKCE dance and the keychain;
/// this crate only asks for a token per request and reports 401s.
pub trait AuthProvider: Send + Sync {
    /// Current bearer token for `server`, if any.
    fn bearer(&self, server: &str) -> Result<Option<Secret>>;
    /// Called once per request on HTTP 401. Return Ok(true) after refreshing so the request is retried once.
    fn on_unauthorized(&self, server: &str, www_authenticate: Option<&str>) -> Result<bool>;
}
