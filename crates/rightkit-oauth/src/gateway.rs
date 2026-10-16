//! Token exchange through an app's own gateway.
//!
//! Some apps keep the provider client secret server side. They post the authorization code
//! and the PKCE verifier as JSON to a gateway they run, and the gateway completes the
//! provider exchange and returns a JSON token response. [`GatewayExchange::exchange`] does
//! that over `rightkit-http`.
//!
//! Request body: `{"code": ..., "code_verifier": ..., <extra fields>}`.
//! Response body: a JSON object with a string `access_token` and optionally a string
//! `token_type`, an unsigned-integer `expires_in` (seconds), and string `refresh_token` and
//! `scope`. A present field of the wrong type is an error, not silently dropped; unknown
//! fields are ignored. The code, verifier and returned tokens never appear in errors or logs.

use std::time::{Duration, SystemTime};

use rightkit_http::{CancelToken, Client, Request, Secret};
use serde_json::{Map, Value};

use crate::pkce::validate_verifier;
use crate::{provider_or_http, scrub_provider, OAuthError, TokenSet};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest gateway response read; a token response is a few KiB.
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const RESERVED_FIELDS: &[&str] = &["code", "code_verifier"];

/// A configured gateway endpoint.
#[derive(Debug, Clone)]
pub struct GatewayExchange {
    url: String,
    extra: Vec<(String, String)>,
    timeout: Duration,
}

impl GatewayExchange {
    /// `endpoint` must be `https://`; plain `http://` is accepted only for a loopback host
    /// (`127.0.0.1`, `[::1]`, `localhost`), as the crate does elsewhere for tests.
    pub fn new(endpoint: impl Into<String>) -> Result<Self, OAuthError> {
        let url = endpoint.into();
        let loopback_http = ["http://127.0.0.1:", "http://[::1]:", "http://localhost:"]
            .iter()
            .any(|p| url.starts_with(p));
        if !(url.starts_with("https://") || loopback_http) {
            return Err(OAuthError::InvalidConfig(
                "gateway endpoint must use HTTPS (or loopback in tests)".into(),
            ));
        }
        Ok(Self {
            url,
            extra: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
        })
    }

    /// An extra string field sent beside `code` and `code_verifier`, such as `redirect_uri`
    /// or an RFC 8707 `resource`. `code` and `code_verifier` cannot be overridden.
    pub fn with_field(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra.push((name.into(), value.into()));
        self
    }

    /// Total time for the call (default 30 s).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn endpoint(&self) -> &str {
        &self.url
    }

    /// Exchange `code` and the PKCE `verifier` for tokens.
    pub fn exchange(
        &self,
        http: &Client,
        code: &str,
        verifier: &str,
        cancel: Option<&CancelToken>,
    ) -> Result<TokenSet, OAuthError> {
        if code.is_empty() {
            return Err(OAuthError::InvalidConfig(
                "authorization code is empty".into(),
            ));
        }
        validate_verifier(verifier)?;
        let mut body = Map::new();
        for (name, value) in &self.extra {
            if RESERVED_FIELDS.contains(&name.as_str()) || body.contains_key(name) {
                return Err(OAuthError::InvalidConfig(format!(
                    "gateway field {name:?} is reserved or repeated"
                )));
            }
            body.insert(name.clone(), Value::String(value.clone()));
        }
        body.insert("code".into(), Value::String(code.into()));
        body.insert("code_verifier".into(), Value::String(verifier.into()));

        let mut request = Request::post(&self.url)
            .header("Accept", "application/json")
            .redact_value(code)
            .redact_value(verifier)
            .json(&Value::Object(body))?
            .timeout(self.timeout)
            .max_response_bytes(MAX_RESPONSE_BYTES);
        if let Some(token) = cancel {
            request = request.cancel(token.clone());
        }
        let response = http
            .send(&request)
            .map_err(|e| provider_or_http(&request, e))?;
        let value: Value = response.json()?;
        if let Some(error) = value.get("error").and_then(Value::as_str) {
            // A gateway may answer 200 with an error body.
            let e = OAuthError::Provider {
                status: response.status,
                error: error.to_string(),
                description: value
                    .get("error_description")
                    .and_then(Value::as_str)
                    .map(String::from),
            };
            return Err(scrub_provider(&request, e));
        }
        parse_gateway_tokens(&value)
    }
}

/// Strictly typed token response parse. Errors name the field, never its value.
pub(crate) fn parse_gateway_tokens(v: &Value) -> Result<TokenSet, OAuthError> {
    let obj = v
        .as_object()
        .ok_or_else(|| bad("token response is not a JSON object"))?;
    let access = string_field(obj, "access_token")?
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad("token response omitted access_token"))?;
    let token_type = string_field(obj, "token_type")?.unwrap_or_else(|| "Bearer".into());
    let expires_in = match obj.get("expires_in") {
        None | Some(Value::Null) => None,
        Some(n) => {
            Some(Duration::from_secs(n.as_u64().ok_or_else(|| {
                bad("expires_in must be a non-negative integer")
            })?))
        }
    };
    let refresh_token = string_field(obj, "refresh_token")?
        .filter(|s| !s.is_empty())
        .map(Secret::new);
    let scope = string_field(obj, "scope")?;
    Ok(TokenSet {
        access_token: Secret::new(access),
        token_type,
        expires_in,
        refresh_token,
        scope,
        id_token: None,
        obtained_at: SystemTime::now(),
    })
}

fn string_field(obj: &Map<String, Value>, name: &str) -> Result<Option<String>, OAuthError> {
    match obj.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(bad(&format!("{name} must be a string"))),
    }
}

fn bad(message: &str) -> OAuthError {
    OAuthError::Callback(message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_a_full_response() {
        let t = parse_gateway_tokens(&json!({
            "access_token": "a", "token_type": "bearer", "expires_in": 60,
            "refresh_token": "r", "scope": "x y", "extra": [1]
        }))
        .unwrap();
        assert_eq!(t.access_token(), "a");
        assert_eq!(t.token_type, "bearer");
        assert_eq!(t.expires_in, Some(Duration::from_secs(60)));
        assert_eq!(t.refresh_token.unwrap().expose(), "r");
        assert_eq!(t.scope.as_deref(), Some("x y"));
    }

    #[test]
    fn minimal_response_defaults() {
        let t = parse_gateway_tokens(&json!({"access_token": "a"})).unwrap();
        assert_eq!(t.token_type, "Bearer");
        assert!(t.expires_in.is_none() && t.refresh_token.is_none() && t.scope.is_none());
    }

    #[test]
    fn wrong_types_are_errors_without_echoing_values() {
        for v in [
            json!([]),
            json!({}),
            json!({"access_token": ""}),
            json!({"access_token": 5}),
            json!({"access_token": "s3cret-token", "expires_in": "3600"}),
            json!({"access_token": "s3cret-token", "expires_in": -1}),
            json!({"access_token": "s3cret-token", "expires_in": 1.5}),
            json!({"access_token": "s3cret-token", "refresh_token": 7}),
            json!({"access_token": "s3cret-token", "scope": ["a"]}),
            json!({"access_token": "s3cret-token", "token_type": {}}),
        ] {
            let e = parse_gateway_tokens(&v).unwrap_err();
            assert!(!e.to_string().contains("s3cret-token"), "{e}");
        }
    }
}
