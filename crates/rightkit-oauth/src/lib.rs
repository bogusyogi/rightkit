//! OAuth 2.0 sign-in for Right Suite apps: one implementation every app uses.
//!
//! * Loopback authorization code + PKCE: [`OAuthClient::begin_loopback`] binds only to
//!   `127.0.0.1` (an ephemeral port unless the provider registration fixes one), creates a
//!   fresh state and S256 verifier, and returns the authorization URL;
//!   [`OAuthClient::finish_loopback`] checks state before exchanging the code.
//! * Device authorization (RFC 8628): [`OAuthClient::begin_device`] /
//!   [`OAuthClient::finish_device`] for headless, CLI/TUI and phone-assisted sign-in.
//! * [`pkce`], [`callback`] and [`gateway`]: public RFC 7636 S256 helpers, a tolerant
//!   single-use loopback listener (RFC 8252) and a JSON gateway code exchange.
//! * Provider presets ([`OAuthConfig::github`], [`OAuthConfig::google`],
//!   [`OAuthConfig::cloudflare`], [`OAuthConfig::openai`]) carry endpoints only; every app
//!   supplies its own registered client id.
//! * `google` (feature `google`): ID-token verification against Google's keys and
//!   `GoogleSession`, a stored, refreshing Google sign-in for Drive sync.
//! * `credentials` (feature `secrets`) stores tokens in the OS credential store, refreshes
//!   them, and resolves a usable token or a typed "needs sign-in" outcome.
//!
//! Token values never appear in `Debug` or `Display` output.
//! Daemons opt into the `server` module/feature for public HTTPS callbacks.

pub mod browser;
pub mod callback;
#[cfg(feature = "secrets")]
pub mod credentials;
mod device;
pub mod gateway;
#[cfg(feature = "google")]
pub mod google;
pub mod pkce;
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

use std::collections::HashMap;
use std::fmt;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use rightkit_http::{
    percent_decode, percent_encode, CancelToken, Client, HttpError, Request, Secret,
};
use sha2::{Digest, Sha256};

const GOOGLE_AUTHORIZATION_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_DEVICE_URL: &str = "https://oauth2.googleapis.com/device/code";
const GITHUB_AUTHORIZATION_URL: &str = "https://github.com/login/oauth/authorize";
const GITHUB_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const GITHUB_DEVICE_URL: &str = "https://github.com/login/device/code";
const CLOUDFLARE_AUTHORIZATION_URL: &str = "https://dash.cloudflare.com/oauth2/auth";
const CLOUDFLARE_TOKEN_URL: &str = "https://dash.cloudflare.com/oauth2/token";
/// Scopes for Google sign-in plus the hidden per-app Drive folder.
pub const GOOGLE_SIGNIN_SCOPES: [&str; 3] = [
    "openid",
    "email",
    "https://www.googleapis.com/auth/drive.appdata",
];
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
    /// An embedded webview (or any in-app surface) was asked to show a sign-in page. Sign-in
    /// opens the user's default browser (or Android Custom Tabs / iOS
    /// `ASWebAuthenticationSession`) so existing browser sessions are kept. See [`browser`].
    EmbeddedWebviewRefused,
    /// The authorization URL is not one the default browser may be handed (not https, or it
    /// carries whitespace/control/quote characters).
    UnsafeAuthorizationUrl(String),
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
            Self::EmbeddedWebviewRefused => {
                f.write_str("sign-in must open the default browser, never an embedded webview")
            }
            Self::UnsafeAuthorizationUrl(m) => {
                write!(f, "authorization URL refused: {m}")
            }
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

    /// Google sign-in for a desktop app: [`Self::google`] with [`GOOGLE_SIGNIN_SCOPES`]
    /// (openid, email, drive.appdata). `client_id` is the app's own "Desktop app" OAuth client
    /// from app config; never embed one in a library. Google may require the client's secret
    /// at the token endpoint even though a desktop client cannot keep it confidential: add it
    /// with [`Self::with_client_secret`]. Verify the returned ID token with
    /// `google::GoogleIdVerifier` (feature `google`), or use `google::GoogleSession`.
    pub fn google_signin(client_id: impl Into<String>) -> Self {
        Self::google(client_id, GOOGLE_SIGNIN_SCOPES)
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
    /// OpenID Connect ID token (a JWT), when the provider returned one. It is unverified
    /// here; the `google` feature verifies Google's. Server mode never retains it.
    pub id_token: Option<Secret>,
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
            .field("id_token", &self.id_token.as_ref().map(|_| "[redacted]"))
            .field("obtained_at", &self.obtained_at)
            .finish()
    }
}

impl TokenSet {
    pub fn access_token(&self) -> &str {
        self.access_token.expose()
    }

    pub fn is_expired(&self, skew: Duration) -> bool {
        // No lifetime means no deadline; a lifetime whose deadline overflows
        // is unusable and fails closed as expired.
        match self.expires_in {
            None => false,
            Some(d) => match self.obtained_at.checked_add(d) {
                None => true,
                Some(at) => match at.duration_since(SystemTime::now()) {
                    Ok(remaining) => remaining <= skew,
                    Err(_) => true,
                },
            },
        }
    }
}

/// A store for application-selected durable token storage.
pub trait TokenStore: Send + Sync {
    fn load(&self, account: &str) -> Result<Option<TokenSet>, OAuthError>;
    fn save(&self, account: &str, tokens: &TokenSet) -> Result<(), OAuthError>;
    fn delete(&self, account: &str) -> Result<(), OAuthError>;
}

/// A [`TokenStore`] that keeps tokens in process memory only, so a
/// `credentials::Resolver` (feature `secrets`) can run when the app persists tokens elsewhere
/// (its own credential store) or not at all. Nothing is written to disk; tokens are gone when
/// the store drops. A `delete` of an absent account succeeds. Safe to share between threads.
/// Tokens never appear in `Debug` output.
#[derive(Default)]
pub struct MemoryTokenStore {
    tokens: Mutex<HashMap<String, TokenSet>>,
}

impl MemoryTokenStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, TokenSet>> {
        // A panic in another thread cannot leave the map half-updated (each operation is a
        // single insert, get or remove), so a poisoned lock is still safe to use.
        self.tokens.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl fmt::Debug for MemoryTokenStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryTokenStore")
            .field("accounts", &self.map().len())
            .finish()
    }
}

impl TokenStore for MemoryTokenStore {
    fn load(&self, account: &str) -> Result<Option<TokenSet>, OAuthError> {
        Ok(self.map().get(account).cloned())
    }

    fn save(&self, account: &str, tokens: &TokenSet) -> Result<(), OAuthError> {
        self.map().insert(account.to_string(), tokens.clone());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), OAuthError> {
        self.map().remove(account);
        Ok(())
    }
}

/// RFC 7009 `token_type_hint` for [`OAuthClient::revoke`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenTypeHint {
    AccessToken,
    RefreshToken,
}

impl TokenTypeHint {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AccessToken => "access_token",
            Self::RefreshToken => "refresh_token",
        }
    }
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
    redirect_path: String,
}

impl fmt::Debug for LoopbackAuthorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoopbackAuthorization")
            .field("request", &self.request)
            .finish()
    }
}

impl LoopbackAuthorization {
    /// Wait for the one browser callback and return its bare authorization code with the
    /// [`AuthorizationRequest`] (whose [`AuthorizationRequest::code_verifier`] the exchange
    /// needs), without exchanging it: for callers that send the code somewhere else, such as a
    /// gateway or a token request with extra parameters.
    ///
    /// Same rules as [`OAuthClient::finish_loopback`]: only a request on the exact redirect
    /// path ends the flow (other paths, stalled and malformed connections are answered and
    /// ignored until the deadline), `state` is compared in constant time and a wrong or
    /// missing one is [`OAuthError::StateMismatch`], a provider `access_denied` is
    /// [`OAuthError::NeedsReauth`]. `cancel` is checked between connections and ends the wait
    /// with [`OAuthError::Cancelled`] (a connection stalled mid-request delays that by at most
    /// two seconds). The listener closes when this returns.
    pub fn wait_code(
        self,
        timeout: Duration,
        cancel: Option<&CancelToken>,
    ) -> Result<(Secret, AuthorizationRequest), OAuthError> {
        let callback = await_callback(&self.listener, &self.redirect_path, timeout, cancel)?;
        let code = check_callback(callback, &self.request.state)?;
        Ok((Secret::new(code), self.request))
    }
}

pub struct OAuthClient {
    pub(crate) config: OAuthConfig,
    pub(crate) http: Client,
    token_params: Vec<(String, String)>,
    revocation_url: Option<String>,
}

impl OAuthClient {
    pub fn new(config: OAuthConfig, http: Client) -> Result<Self, OAuthError> {
        validate_config(&config)?;
        Ok(Self {
            config,
            http,
            token_params: Vec::new(),
            revocation_url: None,
        })
    }

    /// Append `name=value` to the authorization-code exchange and refresh request bodies,
    /// for example an RFC 8707 `resource` or a `scope` (MCP OAuth sends both). Call again
    /// with the same name to send several values. Values are not redacted in `Debug`: never
    /// put a credential here (use [`OAuthConfig::with_client_secret`]). The parameters the
    /// client itself sets (`grant_type`, `code`, `redirect_uri`, `client_id`,
    /// `code_verifier`, `refresh_token`, `client_secret`) are refused. The `server` module
    /// ignores these parameters.
    pub fn with_token_param(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, OAuthError> {
        let name = name.into();
        validate_token_param_name(&name)?;
        self.token_params.push((name, value.into()));
        Ok(self)
    }

    /// The RFC 7009 revocation endpoint [`Self::revoke`] posts to: HTTPS, or an exact
    /// loopback host (`127.0.0.1`, `[::1]`, `localhost`) over plain HTTP in tests. Userinfo
    /// is refused.
    pub fn with_revocation_url(mut self, url: impl Into<String>) -> Result<Self, OAuthError> {
        let url = url.into();
        check_endpoint(&url, "revocation_url")?;
        self.revocation_url = Some(url);
        Ok(self)
    }

    pub fn config(&self) -> &OAuthConfig {
        &self.config
    }

    /// Bind the callback listener before constructing the authorization URL.
    pub fn begin_loopback(&self) -> Result<LoopbackAuthorization, OAuthError> {
        self.begin_loopback_with(&[])
    }

    /// [`Self::begin_loopback`] plus per-flow authorization parameters (for example an OpenID
    /// Connect `nonce`), appended after the configured ones.
    pub(crate) fn begin_loopback_with(
        &self,
        extra: &[(&str, &str)],
    ) -> Result<LoopbackAuthorization, OAuthError> {
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
        for (k, v) in extra {
            pairs.push((*k, (*v).to_string()));
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
            redirect_path: self.config.redirect.path.clone(),
        })
    }

    /// Wait for one browser callback and exchange its authorization code.
    pub fn finish_loopback(
        &self,
        flow: LoopbackAuthorization,
        timeout: Duration,
    ) -> Result<TokenSet, OAuthError> {
        let (code, request) = flow.wait_code(timeout, None)?;
        self.exchange_code(&request, code.expose())
    }

    /// Desktop sign-in through the user's default browser: bind the loopback listener, open the
    /// authorization URL (PKCE `S256`, `state`) with `opener`, wait for the single redirect,
    /// exchange the code. An opener on an embedded webview is refused before anything is
    /// bound or opened. Use [`browser::SystemBrowser`] for the OS default handler.
    pub fn sign_in_with_browser(
        &self,
        opener: &dyn browser::BrowserOpener,
        timeout: Duration,
    ) -> Result<TokenSet, OAuthError> {
        self.sign_in_with_browser_extra(opener, timeout, &[])
    }

    pub(crate) fn sign_in_with_browser_extra(
        &self,
        opener: &dyn browser::BrowserOpener,
        timeout: Duration,
        extra: &[(&str, &str)],
    ) -> Result<TokenSet, OAuthError> {
        opener.surface().require_allowed()?;
        let flow = self.begin_loopback_with(extra)?;
        browser::check_authorization_url(&flow.request.authorization_url)?;
        opener.open(&flow.request.authorization_url)?;
        self.finish_loopback(flow, timeout)
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
        let mut pairs = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", authorization.redirect_uri.as_str()),
            ("client_id", self.config.client_id.as_str()),
            ("code_verifier", authorization.verifier.as_str()),
        ];
        self.push_token_params(&mut pairs);
        self.token_request(&pairs, None)
    }

    /// Refresh an access token. A missing refresh token is a reauthorization requirement.
    pub fn refresh(&self, current: &TokenSet) -> Result<TokenSet, OAuthError> {
        let refresh = current
            .refresh_token
            .as_ref()
            .ok_or(OAuthError::NeedsReauth)?;
        let mut pairs = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.expose()),
            ("client_id", self.config.client_id.as_str()),
        ];
        self.push_token_params(&mut pairs);
        let mut fresh = self.token_request(&pairs, Some(refresh))?;
        // A refresh response may omit the ID token (OpenID Connect Core 12.2); keep the one
        // the sign-in produced rather than losing the account identity.
        if fresh.id_token.is_none() {
            fresh.id_token = current.id_token.clone();
        }
        Ok(fresh)
    }

    /// Append the configured extra token-endpoint parameters (RFC 8707 `resource`, `scope`).
    fn push_token_params<'a>(&'a self, pairs: &mut Vec<(&'a str, &'a str)>) {
        for (name, value) in &self.token_params {
            pairs.push((name.as_str(), value.as_str()));
        }
    }

    /// Revoke `token` at the configured [`Self::with_revocation_url`] (RFC 7009). Sends
    /// `token`, the optional `token_type_hint`, `client_id`, the client secret when one is
    /// configured, and the configured [`Self::with_token_param`] (so an RFC 8707 `resource`
    /// rides along). Success is any 2xx answer; per RFC 7009 a token the provider no longer
    /// knows is also a success. The token is registered as a request secret, so no error text
    /// repeats it. Without a configured revocation URL this is
    /// [`OAuthError::InvalidConfig`] and nothing is sent.
    pub fn revoke(&self, token: &Secret, hint: Option<TokenTypeHint>) -> Result<(), OAuthError> {
        let url = self
            .revocation_url
            .as_deref()
            .ok_or_else(|| OAuthError::InvalidConfig("revocation_url is not configured".into()))?;
        if token.expose().is_empty() {
            return Err(OAuthError::InvalidConfig("token is empty".into()));
        }
        let mut pairs: Vec<(&str, &str)> = vec![("token", token.expose())];
        if let Some(hint) = hint {
            pairs.push(("token_type_hint", hint.as_str()));
        }
        pairs.push(("client_id", self.config.client_id.as_str()));
        if let Some(secret) = &self.config.client_secret {
            pairs.push(("client_secret", secret.expose()));
        }
        self.push_token_params(&mut pairs);
        let request = Request::post(url)
            .header("Accept", "application/json")
            .secret_form(&pairs, SECRET_FORM_FIELDS);
        let response = self
            .http
            .send(&request)
            .map_err(|e| provider_or_http(&request, e))?;
        // A 2xx body is normally empty. A provider that reports an error with HTTP 200 (as
        // GitHub does on its token endpoint) must not be read as success.
        if let Ok(value) = response.json::<serde_json::Value>() {
            if let Some(error) = value.get("error").and_then(|e| e.as_str()) {
                return Err(scrub_provider(
                    &request,
                    error_from(response.status, error, &value),
                ));
            }
        }
        Ok(())
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
        let request = Request::post(&self.config.token_url)
            .header("Accept", "application/json")
            .secret_form(&refs, SECRET_FORM_FIELDS);
        let response = self
            .http
            .send(&request)
            .map_err(|e| provider_or_http(&request, e))?;
        let value: serde_json::Value = response.json()?;
        if let Some(error) = value.get("error").and_then(|e| e.as_str()) {
            return Err(scrub_provider(
                &request,
                error_from(response.status, error, &value),
            ));
        }
        parse_tokens(&value, old_refresh)
    }
}

fn validate_config(c: &OAuthConfig) -> Result<(), OAuthError> {
    if c.client_id.trim().is_empty() {
        return Err(OAuthError::InvalidConfig("client_id is empty".into()));
    }
    check_endpoint(&c.authorization_url, "authorization_url")?;
    check_endpoint(&c.token_url, "token_url")?;
    if let Some(d) = &c.device_authorization_url {
        check_endpoint(d, "device_authorization_url")?;
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

fn validate_token_param_name(name: &str) -> Result<(), OAuthError> {
    if name.is_empty() || name.bytes().any(|b| b <= 0x20 || b == 0x7f) {
        return Err(OAuthError::InvalidConfig(
            "token parameter name must be non-empty with no whitespace or control characters"
                .into(),
        ));
    }
    if RESERVED_TOKEN_PARAMS
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(name))
    {
        return Err(OAuthError::InvalidConfig(format!(
            "token parameter {name:?} is set by the client and cannot be overridden"
        )));
    }
    Ok(())
}

/// Validate an endpoint URL by parsing its authority, not by prefix. The scheme must be
/// `https`, or `http` with the exact host `127.0.0.1`, `[::1]` or `localhost`; userinfo
/// (`user@`), backslashes, whitespace and control characters are refused, so
/// `http://localhost:80@attacker.example/` cannot pass as loopback.
pub(crate) fn check_endpoint(url: &str, what: &str) -> Result<(), OAuthError> {
    let invalid = |why: &str| OAuthError::InvalidConfig(format!("{what} {why}"));
    if url
        .bytes()
        .any(|b| b <= 0x20 || b == 0x7f || b == b'\\' || !b.is_ascii())
    {
        return Err(invalid(
            "holds whitespace, control, backslash or non-ASCII characters",
        ));
    }
    let (https, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(invalid("must use HTTPS (or loopback in tests)"));
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.contains('@') {
        return Err(invalid("must not carry userinfo"));
    }
    let (host, port) = if let Some(after) = authority.strip_prefix('[') {
        let close = after
            .find(']')
            .ok_or_else(|| invalid("has an unterminated IPv6 host"))?;
        let host = &authority[..close + 2];
        let tail = &after[close + 1..];
        match tail {
            "" => (host, None),
            t => (
                host,
                Some(
                    t.strip_prefix(':')
                        .ok_or_else(|| invalid("has a malformed authority"))?,
                ),
            ),
        }
    } else {
        match authority.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        }
    };
    if let Some(p) = port {
        let digits = !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit());
        if !digits || !matches!(p.parse::<u32>(), Ok(1..=65535)) {
            return Err(invalid("has an invalid port"));
        }
    }
    if https {
        let bracketed = host.starts_with('[');
        let ok = if bracketed {
            host.len() > 2
                && host[1..host.len() - 1]
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.')
        } else {
            !host.is_empty()
                && host.len() <= 253
                && host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
        };
        if !ok {
            return Err(invalid("has an invalid host"));
        }
        return Ok(());
    }
    if matches!(host, "127.0.0.1" | "[::1]" | "localhost") {
        Ok(())
    } else {
        Err(invalid("must use HTTPS (or loopback in tests)"))
    }
}

/// Token-endpoint fields the client sets itself; a configured extra parameter may not repeat
/// or override them (a second `code_verifier` or `redirect_uri` would be ambiguous), and
/// `token`, `token_type_hint` are the revocation request's own.
const RESERVED_TOKEN_PARAMS: &[&str] = &[
    "grant_type",
    "code",
    "redirect_uri",
    "client_id",
    "client_secret",
    "code_verifier",
    "refresh_token",
    "token",
    "token_type_hint",
];

/// Form fields whose values are credentials: they are registered as secrets on the request
/// so no error text, ours or the provider's, repeats them.
pub(crate) const SECRET_FORM_FIELDS: &[&str] = &[
    "client_secret",
    "refresh_token",
    "device_code",
    "code",
    "code_verifier",
    "token",
];

/// Remove the request's secret values from a provider error's `error` and description. A
/// provider may echo a submitted credential, and an HTTP 200 answer reaches us as a parsed
/// body rather than as an `HttpError` that the HTTP layer would have scrubbed.
pub(crate) fn scrub_provider(request: &Request, e: OAuthError) -> OAuthError {
    match e {
        OAuthError::Provider {
            status,
            error,
            description,
        } => OAuthError::Provider {
            status,
            error: request.scrub(&error),
            description: description.map(|d| request.scrub(&d)),
        },
        other => other,
    }
}

fn provider_or_http(request: &Request, e: HttpError) -> OAuthError {
    match e {
        HttpError::Status { status, body, .. } => {
            let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            let error = v
                .get("error")
                .and_then(|x| x.as_str())
                .unwrap_or("token_request_failed")
                .to_string();
            scrub_provider(request, error_from(status, &error, &v))
        }
        other => OAuthError::Http(other),
    }
}

/// A token-endpoint error. A dead grant (`invalid_grant`; GitHub's `bad_refresh_token`) means
/// the user must sign in again.
pub(crate) fn error_from(status: u16, error: &str, v: &serde_json::Value) -> OAuthError {
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
        id_token: v
            .get("id_token")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .map(Secret::new),
        obtained_at: SystemTime::now(),
    })
}

/// Serve `listener` until the first request on exactly `path` (see
/// [`LoopbackAuthorization::wait_code`]). Other paths, stalled and malformed connections are
/// answered and ignored until the deadline.
fn await_callback(
    listener: &TcpListener,
    path: &str,
    timeout: Duration,
    cancel: Option<&CancelToken>,
) -> Result<Callback, OAuthError> {
    listener
        .set_nonblocking(true)
        .map_err(|e| OAuthError::Callback(e.to_string()))?;
    let started = std::time::Instant::now();
    // Single use: the first request on the exact redirect path ends the flow. Requests on
    // other paths (favicon, probes) and stalled or malformed connections are answered and
    // ignored until the deadline, so none of them can end the flow.
    loop {
        if cancel.is_some_and(CancelToken::is_cancelled) {
            return Err(OAuthError::Cancelled);
        }
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(OAuthError::Callback(
                    "timed out waiting for browser callback".into(),
                ));
            }
            Err(e) => return Err(OAuthError::Callback(format!("accept callback: {e}"))),
        };
        // One absolute deadline for the whole request line, never per read. With a cancel
        // token the budget is short so a stalled connection cannot hold cancellation for long.
        let mut remaining = timeout
            .saturating_sub(started.elapsed())
            .clamp(Duration::from_millis(250), CALLBACK_READ_MAX);
        if cancel.is_some() {
            remaining = remaining.min(Duration::from_secs(2));
        }
        match read_callback(stream, remaining) {
            Ok(cb) if cb.path == path => return Ok(cb),
            Ok(_) if started.elapsed() < timeout => continue,
            Ok(_) => {
                return Err(OAuthError::Callback(
                    "timed out waiting for browser callback".into(),
                ))
            }
            Err(e) => return Err(e),
        }
    }
}

/// Check a callback against the expected `state` and return its authorization code.
fn check_callback(callback: Callback, expected_state: &str) -> Result<String, OAuthError> {
    let state_ok = callback
        .state
        .as_deref()
        .is_some_and(|s| pkce::constant_time_eq(s.as_bytes(), expected_state.as_bytes()));
    if !state_ok {
        return Err(OAuthError::StateMismatch);
    }
    if let Some(e) = callback.error {
        if e == "access_denied" {
            return Err(OAuthError::NeedsReauth);
        }
        return Err(OAuthError::Callback(e));
    }
    callback
        .code
        .ok_or_else(|| OAuthError::Callback("callback did not contain a code".into()))
}

struct Callback {
    /// Request path without the query.
    path: String,
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
        path: target
            .split_once('?')
            .map(|(p, _)| p)
            .unwrap_or(target)
            .to_owned(),
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
