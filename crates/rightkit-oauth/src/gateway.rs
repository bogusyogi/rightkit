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
//!
//! A gateway often returns more than the standard fields (`expires_at`, `provider`,
//! `user_id`). [`GatewayExchange::exchange_tokens`] returns them in [`GatewayTokens::extra`]
//! and [`GatewayExchange::exchange_raw`] returns the whole object. A gateway that also
//! refreshes is configured with [`GatewayExchange::with_refresh_endpoint`] and called with
//! [`GatewayExchange::refresh`] (`{"refresh_token": ..., <extra fields>}`; HTTP 401 means the
//! grant is gone and is [`OAuthError::NeedsReauth`]).

use std::fmt;
use std::time::{Duration, SystemTime};

use rightkit_http::{CancelToken, Client, HttpError, Request, Secret};
use serde_json::{Map, Value};

use crate::pkce::validate_verifier;
use crate::{error_from, provider_or_http, scrub_provider, OAuthError, TokenSet};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest gateway response read; a token response is a few KiB.
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const RESERVED_FIELDS: &[&str] = &["code", "code_verifier"];
const RESERVED_REFRESH_FIELDS: &[&str] = &["refresh_token"];
/// Fields [`parse_gateway_tokens`] reads; any other field of a response is "extra".
const STANDARD_FIELDS: &[&str] = &[
    "access_token",
    "token_type",
    "expires_in",
    "refresh_token",
    "scope",
];

/// A configured gateway endpoint.
#[derive(Debug, Clone)]
pub struct GatewayExchange {
    url: String,
    refresh_url: Option<String>,
    extra: Vec<(String, String)>,
    timeout: Duration,
}

/// A gateway token response: the parsed [`TokenSet`] plus every other field the gateway
/// returned (`expires_at`, `provider`, `user_id`, ...), unmodified. Values in `extra` may be
/// credentials, so `Debug` prints only their names.
#[derive(Clone)]
pub struct GatewayTokens {
    pub token_set: TokenSet,
    pub extra: Map<String, Value>,
}

impl fmt::Debug for GatewayTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GatewayTokens")
            .field("token_set", &self.token_set)
            .field("extra_fields", &self.extra.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// `https://`, or plain `http://` only for an exact loopback host (tests); userinfo refused.
fn check_endpoint(url: &str, what: &str) -> Result<(), OAuthError> {
    crate::check_endpoint(url, what)
}

impl GatewayExchange {
    /// `endpoint` must be `https://`; plain `http://` is accepted only for a loopback host
    /// (`127.0.0.1`, `[::1]`, `localhost`), as the crate does elsewhere for tests.
    pub fn new(endpoint: impl Into<String>) -> Result<Self, OAuthError> {
        let url = endpoint.into();
        check_endpoint(&url, "gateway endpoint")?;
        Ok(Self {
            url,
            refresh_url: None,
            extra: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
        })
    }

    /// The gateway's refresh endpoint, used by [`Self::refresh`]. Same URL rules as
    /// [`Self::new`].
    pub fn with_refresh_endpoint(
        mut self,
        endpoint: impl Into<String>,
    ) -> Result<Self, OAuthError> {
        let url = endpoint.into();
        check_endpoint(&url, "gateway refresh endpoint")?;
        self.refresh_url = Some(url);
        Ok(self)
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
        let value = self.exchange_raw(http, code, verifier, cancel)?;
        parse_gateway_tokens(&value)
    }

    /// [`Self::exchange`] that also returns the gateway's non-standard fields.
    pub fn exchange_tokens(
        &self,
        http: &Client,
        code: &str,
        verifier: &str,
        cancel: Option<&CancelToken>,
    ) -> Result<GatewayTokens, OAuthError> {
        let value = self.exchange_raw(http, code, verifier, cancel)?;
        split_tokens(&value, None)
    }

    /// [`Self::exchange`] returning the gateway's whole JSON object. It is checked to be an
    /// object with no `error`, but not otherwise parsed. The value holds the tokens in
    /// plain text and its `Debug` output prints them: do not log it.
    pub fn exchange_raw(
        &self,
        http: &Client,
        code: &str,
        verifier: &str,
        cancel: Option<&CancelToken>,
    ) -> Result<Value, OAuthError> {
        if code.is_empty() {
            return Err(OAuthError::InvalidConfig(
                "authorization code is empty".into(),
            ));
        }
        validate_verifier(verifier)?;
        let mut body = fields_body(
            self.extra.iter().map(|(k, v)| (k.as_str(), v.as_str())),
            RESERVED_FIELDS,
        )?;
        body.insert("code".into(), Value::String(code.into()));
        body.insert("code_verifier".into(), Value::String(verifier.into()));
        self.post(http, &self.url, body, &[code, verifier], false, cancel)
    }

    /// Refresh through the gateway's refresh endpoint (see [`Self::with_refresh_endpoint`]):
    /// posts `{"refresh_token": ..., <extra_fields>}` as JSON (for example
    /// `[("provider", "nous")]`; the configured [`Self::with_field`] values are exchange-only
    /// and are not sent). HTTP 401, and an `invalid_grant` error, mean the grant is gone:
    /// [`OAuthError::NeedsReauth`]. When the response omits a refresh token the one passed in
    /// is kept in the returned set. `extra_fields` cannot repeat a name or set `refresh_token`.
    pub fn refresh(
        &self,
        http: &Client,
        refresh_token: &Secret,
        extra_fields: &[(&str, &str)],
        cancel: Option<&CancelToken>,
    ) -> Result<GatewayTokens, OAuthError> {
        let url = self.refresh_url.as_deref().ok_or_else(|| {
            OAuthError::InvalidConfig("gateway refresh endpoint is not configured".into())
        })?;
        if refresh_token.expose().is_empty() {
            return Err(OAuthError::InvalidConfig("refresh token is empty".into()));
        }
        let mut body = fields_body(extra_fields.iter().copied(), RESERVED_REFRESH_FIELDS)?;
        body.insert(
            "refresh_token".into(),
            Value::String(refresh_token.expose().into()),
        );
        let value = self.post(http, url, body, &[refresh_token.expose()], true, cancel)?;
        split_tokens(&value, Some(refresh_token))
    }

    /// POST `body` as JSON to `url` and return the response object. `secrets` are scrubbed
    /// from every error. A 401 is a dead grant when `unauthorized_is_reauth`.
    fn post(
        &self,
        http: &Client,
        url: &str,
        body: Map<String, Value>,
        secrets: &[&str],
        unauthorized_is_reauth: bool,
        cancel: Option<&CancelToken>,
    ) -> Result<Value, OAuthError> {
        let mut request = Request::post(url).header("Accept", "application/json");
        for secret in secrets {
            request = request.redact_value(*secret);
        }
        let mut request = request
            .json(&Value::Object(body))?
            .timeout(self.timeout)
            .max_response_bytes(MAX_RESPONSE_BYTES);
        if let Some(token) = cancel {
            request = request.cancel(token.clone());
        }
        let response = http.send(&request).map_err(|e| match e {
            HttpError::Status { status: 401, .. } if unauthorized_is_reauth => {
                OAuthError::NeedsReauth
            }
            other => provider_or_http(&request, other),
        })?;
        let value: Value = response.json()?;
        if let Some(error) = value.get("error").and_then(Value::as_str) {
            // A gateway may answer 200 with an error body; a dead grant is NeedsReauth.
            return Err(scrub_provider(
                &request,
                error_from(response.status, error, &value),
            ));
        }
        if !value.is_object() {
            return Err(bad("token response is not a JSON object"));
        }
        Ok(value)
    }
}

/// Build a JSON body from string fields, refusing reserved and repeated names.
fn fields_body<'a>(
    fields: impl Iterator<Item = (&'a str, &'a str)>,
    reserved: &[&str],
) -> Result<Map<String, Value>, OAuthError> {
    let mut body = Map::new();
    for (name, value) in fields {
        if reserved.contains(&name) || body.contains_key(name) {
            return Err(OAuthError::InvalidConfig(format!(
                "gateway field {name:?} is reserved or repeated"
            )));
        }
        body.insert(name.to_string(), Value::String(value.to_string()));
    }
    Ok(body)
}

/// Parse the token fields and keep the rest. `fallback_refresh` is the refresh token to keep
/// when the response carries none.
fn split_tokens(
    value: &Value,
    fallback_refresh: Option<&Secret>,
) -> Result<GatewayTokens, OAuthError> {
    let mut token_set = parse_gateway_tokens(value)?;
    if token_set.refresh_token.is_none() {
        token_set.refresh_token = fallback_refresh.cloned();
    }
    let extra = value
        .as_object()
        .map(|obj| {
            obj.iter()
                .filter(|(k, _)| !STANDARD_FIELDS.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default();
    Ok(GatewayTokens { token_set, extra })
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
