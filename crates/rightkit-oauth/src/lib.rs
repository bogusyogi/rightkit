//! OAuth 2.0 sign-in for Right Suite apps: one implementation every app uses.
//!
//! * Loopback authorization code + PKCE: [`OAuthClient::begin_loopback`] binds only to
//!   `127.0.0.1` (an ephemeral port unless the provider registration fixes one), creates a
//!   fresh state and S256 verifier, and returns the authorization URL;
//!   [`OAuthClient::finish_loopback`] checks state before exchanging the code.
//! * Device authorization (RFC 8628): [`OAuthClient::begin_device`] /
//!   [`OAuthClient::finish_device`] for headless, CLI/TUI and phone-assisted sign-in.
//! * Provider presets ([`OAuthConfig::github`], [`OAuthConfig::google`],
//!   [`OAuthConfig::cloudflare`], [`OAuthConfig::openai`]) carry endpoints only; every app
//!   supplies its own registered client id.
//! * `credentials` (feature `secrets`) stores tokens in the OS credential store, refreshes
//!   them, and resolves a usable token or a typed "needs sign-in" outcome.
//!
//! Token values never appear in `Debug` or `Display` output.
//! Daemons opt into the `server` module/feature for public HTTPS callbacks.

#[cfg(feature = "secrets")]
pub mod credentials;
mod device;
#[cfg(feature = "server")]
pub mod server;
#[cfg(feature = "system-sources")]
pub mod system;
#[cfg(any(feature = "secrets", feature = "server"))]
mod token_codec;

pub use device::{
    Clock, DeviceAuthorization, DevicePoll, DeviceWait, SystemClock, DEFAULT_INTERVAL,
    MINIMUM_INTERVAL, SLOW_DOWN_INCREMENT,
};

use std::fmt;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, SystemTime};

use rightkit_http::{percent_decode, percent_encode, Client, HttpError, Request, Secret};
use sha2::{Digest, Sha256};

const GOOGLE_AUTHORIZATION_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_DEVICE_URL: &str = "https://oauth2.googleapis.com/device/code";
const GITHUB_AUTHORIZATION_URL: &str = "https://github.com/login/oauth/authorize";
const GITHUB_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const GITHUB_DEVICE_URL: &str = "https://github.com/login/device/code";
const CLOUDFLARE_AUTHORIZATION_URL: &str = "https://dash.cloudflare.com/oauth2/auth";
const CLOUDFLARE_TOKEN_URL: &str = "https://dash.cloudflare.com/oauth2/token";
const OPENAI_AUTHORIZATION_URL: &str = "https://auth.openai.com/oauth/authorize";
const OPENAI_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

#[derive(Debug)]
#[non_exhaustive]
pub enum OAuthError {
    InvalidConfig(String),
    Http(HttpError),
    Callback(String),
    StateMismatch,
    NeedsReauth,
    Provider {
        status: u16,
        error: String,
        description: Option<String>,
    },
    /// The device code expired before the user approved it.
    DeviceCodeExpired,
    /// The user declined the device authorization.
    AccessDenied,
    /// The caller cancelled the flow.
    Cancelled,
    /// The OS credential store failed (never carries credential bytes).
    Storage(String),
}

impl fmt::Display for OAuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(m) => write!(f, "invalid OAuth configuration: {m}"),
            Self::Http(e) => write!(f, "OAuth HTTP error: {e}"),
            Self::Callback(m) => write!(f, "OAuth callback error: {m}"),
            Self::StateMismatch => f.write_str("OAuth callback state mismatch"),
            Self::NeedsReauth => {
                f.write_str("OAuth authorization is no longer valid; reauthorization required")
            }
            Self::Provider {
                status,
                error,
                description,
            } => {
                write!(f, "OAuth provider error {error} (HTTP {status})")?;
                if let Some(d) = description {
                    write!(f, ": {d}")?;
                }
                Ok(())
            }
            Self::DeviceCodeExpired => {
                f.write_str("the sign-in code expired before it was approved")
            }
            Self::AccessDenied => f.write_str("sign-in was declined"),
            Self::Cancelled => f.write_str("sign-in was cancelled"),
            Self::Storage(m) => write!(f, "credential storage error: {m}"),
        }
    }
}

impl std::error::Error for OAuthError {}

impl From<HttpError> for OAuthError {
    fn from(e: HttpError) -> Self {
        Self::Http(e)
    }
}

/// OAuth provider endpoints and client parameters.
#[derive(Debug, Clone)]
pub struct OAuthConfig {
    pub authorization_url: String,
    pub token_url: String,
    pub client_id: String,
    pub client_secret: Option<Secret>,
    pub scopes: Vec<String>,
    /// Extra query parameters for provider-specific consent behavior.
    pub authorization_params: Vec<(String, String)>,
    /// RFC 8628 device authorization endpoint, when the provider supports device sign-in.
    pub device_authorization_url: Option<String>,
    /// Where the loopback flow listens. Defaults to `127.0.0.1`, an ephemeral port, and
    /// `/oauth/callback`; providers whose registration fixes the redirect set it exactly.
    pub redirect: LoopbackRedirect,
}

/// The loopback redirect the provider registration expects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopbackRedirect {
    /// Host spelled in `redirect_uri`: `127.0.0.1` or `localhost`. The listener always binds
    /// `127.0.0.1` only.
    pub host: String,
    /// `0` picks an ephemeral port.
    pub port: u16,
    pub path: String,
}

impl Default for LoopbackRedirect {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 0,
            path: "/oauth/callback".into(),
        }
    }
}

impl OAuthConfig {
    pub fn new(
        client_id: impl Into<String>,
        authorization_url: impl Into<String>,
        token_url: impl Into<String>,
    ) -> Self {
        Self {
            authorization_url: authorization_url.into(),
            token_url: token_url.into(),
            client_id: client_id.into(),
            client_secret: None,
            scopes: Vec::new(),
            authorization_params: Vec::new(),
            device_authorization_url: None,
            redirect: LoopbackRedirect::default(),
        }
    }

    /// Google (incl. Gmail scopes): loopback with offline consent, plus device sign-in.
    pub fn google(
        client_id: impl Into<String>,
        scopes: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let mut c = Self::new(client_id, GOOGLE_AUTHORIZATION_URL, GOOGLE_TOKEN_URL);
        c.scopes = scopes.into_iter().map(Into::into).collect();
        c.authorization_params
            .push(("access_type".into(), "offline".into()));
        c.authorization_params
            .push(("prompt".into(), "consent".into()));
        c.device_authorization_url = Some(GOOGLE_DEVICE_URL.into());
        c
    }

    /// GitHub, for the app's own GitHub App (or OAuth App) registration. GitHub Apps with
    /// expiring user tokens return a rotating refresh grant; permissions come from the App,
    /// so `scopes` is usually empty. Device sign-in is supported.
    pub fn github(
        client_id: impl Into<String>,
        scopes: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let mut c = Self::new(client_id, GITHUB_AUTHORIZATION_URL, GITHUB_TOKEN_URL);
        c.scopes = scopes.into_iter().map(Into::into).collect();
        c.device_authorization_url = Some(GITHUB_DEVICE_URL.into());
        c
    }

    /// Cloudflare dashboard OAuth. Set the redirect the app's registration names with
    /// [`Self::with_redirect`].
    pub fn cloudflare(
        client_id: impl Into<String>,
        scopes: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let mut c = Self::new(
            client_id,
            CLOUDFLARE_AUTHORIZATION_URL,
            CLOUDFLARE_TOKEN_URL,
        );
        c.scopes = scopes.into_iter().map(Into::into).collect();
        c
    }

    /// OpenAI / ChatGPT account sign-in. Its registrations fix the redirect (a `localhost`
    /// port and path); set it with [`Self::with_redirect`].
    pub fn openai(
        client_id: impl Into<String>,
        scopes: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let mut c = Self::new(client_id, OPENAI_AUTHORIZATION_URL, OPENAI_TOKEN_URL);
        c.scopes = scopes.into_iter().map(Into::into).collect();
        c
    }

    pub fn with_redirect(
        mut self,
        host: impl Into<String>,
        port: u16,
        path: impl Into<String>,
    ) -> Self {
        self.redirect = LoopbackRedirect {
            host: host.into(),
            port,
            path: path.into(),
        };
        self
    }

    pub fn with_device_authorization_url(mut self, url: impl Into<String>) -> Self {
        self.device_authorization_url = Some(url.into());
        self
    }

    pub fn with_client_secret(mut self, secret: impl Into<String>) -> Self {
        self.client_secret = Some(Secret::new(secret));
        self
    }

    pub fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.scopes.push(scope.into());
        self
    }

    pub fn with_authorization_param(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.authorization_params.push((name.into(), value.into()));
        self
    }
}

/// Tokens returned by an OAuth provider. Secret values never appear in Debug or Display output.
#[derive(Clone)]
pub struct TokenSet {
    pub access_token: Secret,
    pub token_type: String,
    pub expires_in: Option<Duration>,
    pub refresh_token: Option<Secret>,
    pub scope: Option<String>,
    pub obtained_at: SystemTime,
}

impl fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &self.access_token)
            // Provider-controlled metadata may echo credential bytes too.
            .field("token_type", &"[redacted]")
            .field("expires_in", &self.expires_in)
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[redacted]"),
            )
            .field("scope", &self.scope.as_ref().map(|_| "[redacted]"))
            .field("obtained_at", &self.obtained_at)
            .finish()
    }
}

impl TokenSet {
    pub fn access_token(&self) -> &str {
        self.access_token.expose()
    }

    pub fn is_expired(&self, skew: Duration) -> bool {
        self.expires_in
            .and_then(|d| self.obtained_at.checked_add(d))
            .map(|at| match at.duration_since(SystemTime::now()) {
                Ok(remaining) => remaining <= skew,
                Err(_) => true,
            })
            .unwrap_or(false)
    }
}

/// A store for application-selected durable token storage.
pub trait TokenStore: Send + Sync {
    fn load(&self, account: &str) -> Result<Option<TokenSet>, OAuthError>;
    fn save(&self, account: &str, tokens: &TokenSet) -> Result<(), OAuthError>;
    fn delete(&self, account: &str) -> Result<(), OAuthError>;
}

/// State created before opening the user's browser.
pub struct AuthorizationRequest {
    pub authorization_url: String,
    pub state: String,
    pub redirect_uri: String,
    verifier: String,
}

impl fmt::Debug for AuthorizationRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthorizationRequest")
            .field("authorization_url", &self.authorization_url)
            .field("state", &self.state)
            .field("redirect_uri", &self.redirect_uri)
            .field("verifier", &"[redacted]")
            .finish()
    }
}

impl AuthorizationRequest {
    pub fn code_verifier(&self) -> &str {
        &self.verifier
    }
}

/// A pending callback listener. Keep it alive while the browser is completing authorization.
pub struct LoopbackAuthorization {
    pub request: AuthorizationRequest,
    listener: TcpListener,
}

impl fmt::Debug for LoopbackAuthorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoopbackAuthorization")
            .field("request", &self.request)
            .finish()
    }
}

pub struct OAuthClient {
    pub(crate) config: OAuthConfig,
    pub(crate) http: Client,
}

impl OAuthClient {
    pub fn new(config: OAuthConfig, http: Client) -> Result<Self, OAuthError> {
        validate_config(&config)?;
        Ok(Self { config, http })
    }

    pub fn config(&self) -> &OAuthConfig {
        &self.config
    }

    /// Bind the callback listener before constructing the authorization URL.
    pub fn begin_loopback(&self) -> Result<LoopbackAuthorization, OAuthError> {
        let listener = TcpListener::bind(("127.0.0.1", self.config.redirect.port))
            .map_err(|e| OAuthError::Callback(format!("bind loopback listener: {e}")))?;
        listener
            .set_nonblocking(false)
            .map_err(|e| OAuthError::Callback(format!("configure loopback listener: {e}")))?;
        let port = listener
            .local_addr()
            .map_err(|e| OAuthError::Callback(e.to_string()))?
            .port();
        let redirect_uri = format!(
            "http://{}:{port}{}",
            self.config.redirect.host, self.config.redirect.path
        );
        let state = random_urlsafe(32)?;
        let verifier = random_urlsafe(48)?;
        let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
        let mut pairs = vec![
            ("response_type", "code".to_string()),
            ("client_id", self.config.client_id.clone()),
            ("redirect_uri", redirect_uri.clone()),
            ("state", state.clone()),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256".into()),
        ];
        if !self.config.scopes.is_empty() {
            pairs.push(("scope", self.config.scopes.join(" ")));
        }
        for (k, v) in &self.config.authorization_params {
            pairs.push((k.as_str(), v.clone()));
        }
        let query = pairs
            .iter()
            .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let authorization_url = format!(
            "{}{}{}",
            self.config.authorization_url,
            if self.config.authorization_url.contains('?') {
                '&'
            } else {
                '?'
            },
            query
        );
        Ok(LoopbackAuthorization {
            request: AuthorizationRequest {
                authorization_url,
                state,
                redirect_uri,
                verifier,
            },
            listener,
        })
    }

    /// Wait for one browser callback and exchange its authorization code.
    pub fn finish_loopback(
        &self,
        flow: LoopbackAuthorization,
        timeout: Duration,
    ) -> Result<TokenSet, OAuthError> {
        flow.listener
            .set_nonblocking(true)
            .map_err(|e| OAuthError::Callback(e.to_string()))?;
        let started = std::time::Instant::now();
        let (stream, _) = loop {
            match flow.listener.accept() {
                Ok(pair) => break pair,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        && started.elapsed() < timeout =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    return Err(OAuthError::Callback(
                        "timed out waiting for browser callback".into(),
                    ));
                }
                Err(e) => return Err(OAuthError::Callback(format!("accept callback: {e}"))),
            }
        };
        // One absolute deadline for the whole request line, never per read.
        let remaining = timeout
            .saturating_sub(started.elapsed())
            .clamp(Duration::from_millis(250), CALLBACK_READ_MAX);
        let callback = read_callback(stream, remaining)?;
        if callback.state.as_deref() != Some(flow.request.state.as_str()) {
            return Err(OAuthError::StateMismatch);
        }
        if let Some(e) = callback.error {
            if e == "access_denied" {
                return Err(OAuthError::NeedsReauth);
            }
            return Err(OAuthError::Callback(e));
        }
        let code = callback
            .code
            .ok_or_else(|| OAuthError::Callback("callback did not contain a code".into()))?;
        self.exchange_code(&flow.request, &code)
    }

    pub fn exchange_code(
        &self,
        authorization: &AuthorizationRequest,
        code: &str,
    ) -> Result<TokenSet, OAuthError> {
        if code.is_empty() {
            return Err(OAuthError::InvalidConfig(
                "authorization code is empty".into(),
            ));
        }
        let pairs = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", authorization.redirect_uri.as_str()),
            ("client_id", self.config.client_id.as_str()),
            ("code_verifier", authorization.verifier.as_str()),
        ];
        self.token_request(&pairs, None)
    }

    /// Refresh an access token. A missing refresh token is a reauthorization requirement.
    pub fn refresh(&self, current: &TokenSet) -> Result<TokenSet, OAuthError> {
        let refresh = current
            .refresh_token
            .as_ref()
            .ok_or(OAuthError::NeedsReauth)?;
        let pairs = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.expose()),
            ("client_id", self.config.client_id.as_str()),
        ];
        self.token_request(&pairs, Some(refresh))
    }

    fn token_request(
        &self,
        pairs: &[(&str, &str)],
        old_refresh: Option<&Secret>,
    ) -> Result<TokenSet, OAuthError> {
        let mut owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into()))
            .collect();
        if let Some(secret) = &self.config.client_secret {
            owned.push(("client_secret".into(), secret.expose().into()));
        }
        let refs: Vec<(&str, &str)> = owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        // GitHub answers form-encoded unless asked for JSON, and reports some errors with 200.
        let response = self
            .http
            .send(
                &Request::post(&self.config.token_url)
                    .header("Accept", "application/json")
                    .form(&refs),
            )
            .map_err(provider_or_http)?;
        let value: serde_json::Value = response.json()?;
        if let Some(error) = value.get("error").and_then(|e| e.as_str()) {
            return Err(error_from(response.status, error, &value));
        }
        parse_tokens(&value, old_refresh)
    }
}

fn validate_config(c: &OAuthConfig) -> Result<(), OAuthError> {
    if c.client_id.trim().is_empty() {
        return Err(OAuthError::InvalidConfig("client_id is empty".into()));
    }
    if !(c.authorization_url.starts_with("https://")
        || c.authorization_url.starts_with("http://127.0.0.1:")
        || c.authorization_url.starts_with("http://localhost:"))
    {
        return Err(OAuthError::InvalidConfig(
            "authorization_url must use HTTPS (or loopback in tests)".into(),
        ));
    }
    if !(c.token_url.starts_with("https://")
        || c.token_url.starts_with("http://127.0.0.1:")
        || c.token_url.starts_with("http://localhost:"))
    {
        return Err(OAuthError::InvalidConfig(
            "token_url must use HTTPS (or loopback in tests)".into(),
        ));
    }
    if let Some(d) = &c.device_authorization_url {
        if !(d.starts_with("https://")
            || d.starts_with("http://127.0.0.1:")
            || d.starts_with("http://localhost:"))
        {
            return Err(OAuthError::InvalidConfig(
                "device_authorization_url must use HTTPS (or loopback in tests)".into(),
            ));
        }
    }
    if !matches!(c.redirect.host.as_str(), "127.0.0.1" | "localhost")
        || !c.redirect.path.starts_with('/')
    {
        return Err(OAuthError::InvalidConfig(
            "redirect must be a 127.0.0.1/localhost path".into(),
        ));
    }
    Ok(())
}

fn provider_or_http(e: HttpError) -> OAuthError {
    match e {
        HttpError::Status { status, body, .. } => {
            let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            let error = v
                .get("error")
                .and_then(|x| x.as_str())
                .unwrap_or("token_request_failed")
                .to_string();
            error_from(status, &error, &v)
        }
        other => OAuthError::Http(other),
    }
}

/// A token-endpoint error. A dead grant (`invalid_grant`; GitHub's `bad_refresh_token`) means
/// the user must sign in again.
fn error_from(status: u16, error: &str, v: &serde_json::Value) -> OAuthError {
    if matches!(error, "invalid_grant" | "bad_refresh_token") {
        return OAuthError::NeedsReauth;
    }
    let description = v
        .get("error_description")
        .and_then(|x| x.as_str())
        .map(String::from);
    OAuthError::Provider {
        status,
        error: error.to_string(),
        description,
    }
}

pub(crate) fn parse_tokens(
    v: &serde_json::Value,
    old_refresh: Option<&Secret>,
) -> Result<TokenSet, OAuthError> {
    let access = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| OAuthError::Callback("token response omitted access_token".into()))?;
    let refresh = v
        .get("refresh_token")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(Secret::new)
        .or_else(|| old_refresh.cloned());
    Ok(TokenSet {
        access_token: Secret::new(access),
        token_type: v
            .get("token_type")
            .and_then(|x| x.as_str())
            .unwrap_or("Bearer")
            .to_string(),
        expires_in: v
            .get("expires_in")
            .and_then(|x| x.as_u64())
            .map(Duration::from_secs),
        refresh_token: refresh,
        scope: v.get("scope").and_then(|x| x.as_str()).map(String::from),
        obtained_at: SystemTime::now(),
    })
}

struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// Longest accepted callback request line (method, target, version).
const CALLBACK_LINE_MAX: usize = 8 * 1024;
/// Ceiling on the absolute time one callback connection may take to deliver its request line.
const CALLBACK_READ_MAX: Duration = Duration::from_secs(10);

/// Read one request line (up to `\n`) under an absolute deadline and a size cap.
fn read_request_line(
    stream: &mut TcpStream,
    deadline: std::time::Instant,
) -> Result<String, OAuthError> {
    use std::io::Read;
    let mut line: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Err(OAuthError::Callback(
                "timed out reading callback request".into(),
            ));
        }
        stream
            .set_read_timeout(Some(left))
            .map_err(|e| OAuthError::Callback(e.to_string()))?;
        let n = match stream.read(&mut chunk) {
            Ok(0) => {
                return Err(OAuthError::Callback(
                    "callback connection closed before request line".into(),
                ))
            }
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Err(OAuthError::Callback(
                    "timed out reading callback request".into(),
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(OAuthError::Callback(e.to_string())),
        };
        let end = chunk[..n].iter().position(|b| *b == b'\n');
        let take = end.unwrap_or(n);
        if line.len() + take > CALLBACK_LINE_MAX {
            return Err(OAuthError::Callback(
                "callback request line too long".into(),
            ));
        }
        line.extend_from_slice(&chunk[..take]);
        if end.is_some() {
            return Ok(String::from_utf8_lossy(&line)
                .trim_end_matches('\r')
                .to_string());
        }
    }
}

fn read_callback(mut stream: TcpStream, read_budget: Duration) -> Result<Callback, OAuthError> {
    stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
    stream.set_nonblocking(false).ok();
    let first = read_request_line(&mut stream, std::time::Instant::now() + read_budget)?;
    let target = first
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| OAuthError::Callback("malformed callback request".into()))?;
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
    let mut callback = Callback {
        code: None,
        state: None,
        error: None,
    };
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match percent_decode(key).as_str() {
            "code" => callback.code = Some(percent_decode(value)),
            "state" => callback.state = Some(percent_decode(value)),
            "error" => callback.error = Some(percent_decode(value)),
            _ => {}
        }
    }
    let body = b"Authorization complete. You may close this window.";
    let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    // The reply is a courtesy page: a browser (or client) that already closed its end must not turn
    // a received callback into a failure. Windows reports that as a reset on write; Unix usually doesn't.
    let _ = stream
        .write_all(response.as_bytes())
        .and_then(|()| stream.write_all(body));
    Ok(callback)
}

fn random_urlsafe(bytes: usize) -> Result<String, OAuthError> {
    let mut b = vec![0u8; bytes];
    getrandom::fill(&mut b)
        .map_err(|e| OAuthError::Callback(format!("randomness unavailable: {e}")))?;
    Ok(base64url(&b))
}

fn base64url(bytes: &[u8]) -> String {
    base64_encode(
        bytes,
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_",
        false,
    )
}

/// Standard alphabet with `=` padding (RFC 4648 section 4), for HTTP Basic credentials.
#[cfg(feature = "server")]
fn base64_standard(bytes: &[u8]) -> String {
    base64_encode(
        bytes,
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/",
        true,
    )
}

fn base64_encode(bytes: &[u8], table: &[u8; 64], pad: bool) -> String {
    // Padded length is the upper bound, so the buffer never reallocates (a reallocation
    // would leave an unwiped copy of encoded credentials behind).
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut i = 0;
    while i < bytes.len() {
        let a = bytes[i] as u32;
        let b = bytes.get(i + 1).copied().unwrap_or(0) as u32;
        let c = bytes.get(i + 2).copied().unwrap_or(0) as u32;
        out.push(table[((a >> 2) & 63) as usize] as char);
        out.push(table[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        if i + 1 < bytes.len() {
            out.push(table[(((b & 15) << 2) | (c >> 6)) as usize] as char);
        } else if pad {
            out.push('=');
        }
        if i + 2 < bytes.len() {
            out.push(table[(c & 63) as usize] as char);
        } else if pad {
            out.push('=');
        }
        i += 3;
    }
    out
}

#[cfg(test)]
mod base64_tests {
    use super::*;
    use base64::Engine as _;

    #[test]
    fn encoders_match_the_base64_crate_for_every_tail_length() {
        let data: Vec<u8> = (0..=255u8).cycle().skip(7).take(300).collect();
        for len in 0..data.len().min(70) {
            let slice = &data[..len];
            assert_eq!(
                base64url(slice),
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(slice),
                "url-safe len {len}"
            );
            #[cfg(feature = "server")]
            assert_eq!(
                base64_standard(slice),
                base64::engine::general_purpose::STANDARD.encode(slice),
                "standard len {len}"
            );
        }
        assert_eq!(base64url(&[0xfb, 0xff, 0xfe]), "-__-");
        #[cfg(feature = "server")]
        {
            assert_eq!(base64_standard(&[0xfb, 0xff, 0xfe]), "+//+");
            assert_eq!(base64_standard(b"id:secret"), "aWQ6c2VjcmV0");
            assert_eq!(base64_standard(b"fo"), "Zm8=");
            assert_eq!(base64_standard(b"f"), "Zg==");
        }
    }
}
