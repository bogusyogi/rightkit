mod backoff;
use backoff::gmail_retry_policy;
// Shared Google authed HTTP client. One place owns the cross-cutting concerns
// every Google API call needs:
//   * minting a short-lived access token from a stored refresh token,
//   * 401 -> mint a fresh token and retry once,
//   * 429 / 5xx -> bounded exponential backoff with jitter (honors Retry-After),
//   * a typed error taxonomy so a caller can tell a dead refresh token
//     (re-consent required) from a transient blip.
// Gmail uses this today; Calendar / People / Tasks / Drive will reuse it
// unchanged. All egress lives in Rust — the webview never reaches Google.
//
// Transport (TLS, timeouts, address policy, proxy, retry/backoff mechanics and
// Retry-After parsing) is rightkit-http's AsyncClient. This file keeps only the
// Google-specific policy: which statuses map to which error and the 401 refresh.
//
// Access tokens are rightkit-oauth's: one `AccessTokens` (a Resolver over an in-memory
// TokenStore) per account, shared app-wide by a hash of the OAuth client + refresh
// token, so separately constructed `Google` clients for the same account coalesce
// behind one refresh and reuse the minted token until it nears expiry.
use crate::oauth::AccessTokens;
use rightkit_fs::digest::Sha256Hasher;
use rightkit_http::{
    form_encode, parse_retry_after, AsyncClient, ClientConfig, HttpError, Method, ProxyMode,
    Request, Response,
};
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// Whole-call budget for a JSON request, retries and 401 refresh included. Also the
/// bound `gmail/deadline.rs` applies to its per-request calls.
pub const REQUEST_DEADLINE: Duration = Duration::from_secs(60);

/// Per-attempt budget for bulk transfers (Drive media download and upload), which
/// can legitimately run far longer than a JSON call.
const BULK_TIMEOUT: Duration = Duration::from_secs(600);

/// Response cap for bulk downloads, above rightkit-http's 32 MiB default.
const BULK_MAX_BYTES: usize = 256 * 1024 * 1024;

/// Build a transport for the OS proxy, falling back to the environment proxy.
/// `ProxyMode::System` refuses some Windows setups (a PAC script, per-scheme routes,
/// `<local>`: `SystemProxyUnsupported`); the fallback keeps traffic flowing instead of
/// losing it. The one place that pairing lives, shared by the Google and AI clients.
pub fn build_with_proxy_fallback<T>(
    build: impl Fn(ProxyMode) -> Result<T, HttpError>,
) -> Result<T, HttpError> {
    build(ProxyMode::System).or_else(|_| build(ProxyMode::Environment))
}

/// The one outbound client for Google and translate calls. Built once and shared:
/// rightkit-http owns TLS roots, the address policy, the proxy choice, and timeouts.
/// Redirects are followed up to 10 hops, as the previous client did; rightkit-http
/// never replays credentials to a new host.
pub fn http_client() -> Result<&'static AsyncClient, String> {
    static CLIENT: OnceLock<Result<AsyncClient, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            build_with_proxy_fallback(|proxy| {
                AsyncClient::new(ClientConfig {
                    proxy,
                    max_redirects: 10,
                    ..ClientConfig::default()
                })
            })
            .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| e.clone())
}

/// Map a `rightkit-oauth` refresh failure onto the Google error taxonomy.
fn classify_mint_error(error: rightkit_oauth::OAuthError) -> GoogleError {
    use rightkit_oauth::OAuthError;
    match error {
        OAuthError::NeedsReauth => GoogleError::Auth("invalid_grant".to_string()),
        OAuthError::Provider { status, error, .. } => match status {
            429 => GoogleError::RateLimited(error),
            _ => GoogleError::Http {
                status,
                body: error,
            },
        },
        OAuthError::Http(HttpError::Status { status, body, .. }) => match status {
            429 => GoogleError::RateLimited(body),
            _ => GoogleError::Http { status, body },
        },
        OAuthError::Http(HttpError::Json(m)) => GoogleError::Parse(format!("token: {m}")),
        OAuthError::Http(other) => GoogleError::Transient(format!("token request: {other}")),
        other => GoogleError::Parse(format!("token: {other}")),
    }
}

#[derive(Debug)]
pub enum GoogleError {
    /// Refresh token rejected (invalid_grant) — revoked or expired. The account
    /// must be reconnected; retrying will not help.
    Auth(String),
    /// 429 / quota exhausted after bounded retries.
    RateLimited(String),
    /// Same as `RateLimited`, carrying the server's Retry-After advice so the
    /// outer batch backoff can honour it.
    RateLimitedAfter {
        message: String,
        retry_after: Duration,
    },
    /// Network error or 5xx after bounded retries. Usually transient.
    Transient(String),
    /// A non-idempotent request (POST/PATCH) failed after it may have reached Google (a post-connect
    /// transport failure or a timeout after sending). The outcome is unknown: it must never be
    /// replayed automatically (a send could be duplicated).
    Uncertain(String),
    /// A non-retryable HTTP status (e.g. 400 / 403 / 404). `body` is the API's
    /// error payload, never message content.
    Http { status: u16, body: String },
    /// Response body failed to deserialize.
    Parse(String),
}

impl std::fmt::Display for GoogleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GoogleError::Auth(m) => write!(f, "auth failed (re-consent required): {m}"),
            GoogleError::RateLimited(m) => write!(f, "rate limited: {m}"),
            GoogleError::RateLimitedAfter { message, .. } => {
                write!(f, "rate limited: {message}")
            }
            GoogleError::Transient(m) => write!(f, "transient error: {m}"),
            GoogleError::Uncertain(m) => write!(f, "outcome unknown: {m}"),
            GoogleError::Http { status, body } => write!(f, "http {status}: {body}"),
            GoogleError::Parse(m) => write!(f, "parse error: {m}"),
        }
    }
}

impl GoogleError {
    /// A short, body-free class string for logging / auditing (safety rail #10:
    /// never log bodies or tokens).
    pub fn class(&self) -> &'static str {
        match self {
            GoogleError::Auth(_) => "auth",
            GoogleError::RateLimited(_) | GoogleError::RateLimitedAfter { .. } => "rate_limited",
            GoogleError::Transient(_) => "transient",
            GoogleError::Uncertain(_) => "uncertain",
            GoogleError::Http { .. } => "http",
            GoogleError::Parse(_) => "parse",
        }
    }

    /// True when a non-idempotent call may have taken effect on Google's side: an `Uncertain`
    /// transport failure, or a 5xx answer (Google can accept a send and still answer 5xx). Callers
    /// of a send keep such an outcome out of any automatic retry.
    pub fn delivery_uncertain(&self) -> bool {
        match self {
            GoogleError::Uncertain(_) => true,
            GoogleError::Http { status, .. } => *status >= 500,
            _ => false,
        }
    }
}

impl From<GoogleError> for String {
    fn from(e: GoogleError) -> String {
        e.to_string()
    }
}

type TokenRegistry = Mutex<HashMap<String, Arc<AccessTokens>>>;

fn token_registry() -> &'static TokenRegistry {
    static TOKENS: OnceLock<TokenRegistry> = OnceLock::new();
    TOKENS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn token_cache_key(client_id: &str, client_secret: &str, refresh_token: &str) -> String {
    let mut h = Sha256Hasher::new();
    h.update(client_id.as_bytes());
    h.update(&[0]);
    h.update(client_secret.as_bytes());
    h.update(&[0]);
    h.update(refresh_token.as_bytes());
    h.finalize()
}

/// Drop the in-memory access tokens for a refresh token that has been replaced or
/// deleted. This is best-effort hygiene; access tokens are short lived, but
/// disconnect/re-auth should not leave a stale entry behind.
pub fn forget_cached_access_token(client_id: &str, client_secret: &str, refresh_token: &str) {
    let key = token_cache_key(client_id, client_secret, refresh_token);
    let mut tokens = token_registry().lock().unwrap_or_else(|e| e.into_inner());
    tokens.remove(&key);
}

#[derive(Clone)]
pub struct Google {
    client_id: String,
    client_secret: String,
    refresh_token: String,
}

/// A JSON call: the whole call (retries included) is bounded by `REQUEST_DEADLINE`.
fn json_call(req: Request, access: &str) -> Request {
    req.bearer(access).deadline(REQUEST_DEADLINE)
}

/// A bulk call (Drive media): a long per-attempt budget and a larger body cap.
fn bulk_call(req: Request, access: &str) -> Request {
    req.bearer(access)
        .timeout(BULK_TIMEOUT)
        .max_response_bytes(BULK_MAX_BYTES)
}

/// Serialize a JSON body up front, so request builders can run inside the retry closure.
fn json_body<B: serde::Serialize>(body: &B) -> Result<Vec<u8>, GoogleError> {
    serde_json::to_vec(body).map_err(|e| GoogleError::Parse(e.to_string()))
}

fn json_request(req: Request, bytes: &[u8]) -> Request {
    req.header("Content-Type", "application/json").body(bytes)
}

/// Append `query` pairs to `url`, form-encoded.
fn with_query(url: &str, query: &[(&str, String)]) -> String {
    if query.is_empty() {
        return url.to_string();
    }
    let pairs: Vec<(&str, &str)> = query.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}{}", form_encode(&pairs))
}

fn parse_json<T: DeserializeOwned>(url: &str, resp: &Response) -> Result<T, GoogleError> {
    resp.json::<T>()
        .map_err(|e| GoogleError::Parse(format!("{url}: {e}")))
}

impl Google {
    pub fn new(client_id: String, client_secret: String, refresh_token: String) -> Self {
        Self {
            client_id,
            client_secret,
            refresh_token,
        }
    }

    /// A usable access token: the held one while fresh, else a refreshed one.
    /// Transient failures (network / 5xx / 429) are retried on `gmail_retry_policy`; an
    /// `invalid_grant` is surfaced as `Auth` (re-consent required).
    async fn access_token(&self) -> Result<String, GoogleError> {
        let policy = gmail_retry_policy();
        let mut attempt = 0;
        loop {
            attempt += 1;
            let tokens = self.tokens()?;
            // rightkit-oauth's refresh is blocking, so it runs on the blocking pool; its
            // resolver serializes concurrent refreshes of this account.
            let resolved = tokio::task::spawn_blocking(move || tokens.access_token())
                .await
                .map_err(|e| GoogleError::Transient(format!("token task: {e}")))?;
            let error = match resolved {
                Ok(token) => return Ok(token),
                Err(error) => classify_mint_error(error),
            };
            let retryable = match &error {
                GoogleError::Transient(_) => true,
                GoogleError::RateLimited(_) | GoogleError::RateLimitedAfter { .. } => {
                    policy.is_retryable_status(429)
                }
                GoogleError::Http { status, .. } => policy.is_retryable_status(*status),
                _ => false,
            };
            if retryable && attempt < policy.max_attempts {
                tokio::time::sleep(policy.delay(attempt, None)).await;
                continue;
            }
            return Err(error);
        }
    }

    /// Discard the held access token (a request got 401); the next call refreshes.
    async fn invalidate(&self) -> Result<(), GoogleError> {
        let tokens = self.tokens()?;
        tokio::task::spawn_blocking(move || tokens.invalidate())
            .await
            .map_err(|e| GoogleError::Transient(format!("token task: {e}")))?
            .map_err(classify_mint_error)
    }

    /// This account's shared token holder, created on first use.
    fn tokens(&self) -> Result<Arc<AccessTokens>, GoogleError> {
        let key = self.token_cache_key();
        let mut tokens = token_registry().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(held) = tokens.get(&key) {
            return Ok(held.clone());
        }
        let held = Arc::new(
            AccessTokens::new(
                &self.client_id,
                &self.client_secret,
                &self.refresh_token,
                &key,
            )
            .map_err(classify_mint_error)?,
        );
        tokens.insert(key, held.clone());
        Ok(held)
    }

    fn token_cache_key(&self) -> String {
        token_cache_key(&self.client_id, &self.client_secret, &self.refresh_token)
    }

    /// Authenticated GET returning JSON, with 401-refresh-once + transient retry.
    pub async fn get_json<T: DeserializeOwned>(
        &self,
        url: &str,
        query: &[(&str, String)],
    ) -> Result<T, GoogleError> {
        let full = with_query(url, query);
        let resp = self
            .send_authed(|access| json_call(Request::get(full.clone()), access))
            .await?;
        parse_json(url, &resp)
    }

    /// Authenticated GET returning raw bytes (Drive alt=media / export).
    pub async fn get_bytes(
        &self,
        url: &str,
        query: &[(&str, String)],
    ) -> Result<Vec<u8>, GoogleError> {
        let full = with_query(url, query);
        let resp = self
            .send_authed(|access| bulk_call(Request::get(full.clone()), access))
            .await?;
        Ok(resp.body)
    }

    /// Authenticated POST of a JSON body, discarding the response body.
    pub async fn post_no_content<B: serde::Serialize>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<(), GoogleError> {
        let bytes = json_body(body)?;
        self.send_authed(|access| json_call(json_request(Request::post(url), &bytes), access))
            .await?;
        Ok(())
    }

    /// Authenticated POST of a JSON body, returning the parsed JSON response.
    pub async fn post_json<B: serde::Serialize, T: DeserializeOwned>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T, GoogleError> {
        let bytes = json_body(body)?;
        let resp = self
            .send_authed(|access| json_call(json_request(Request::post(url), &bytes), access))
            .await?;
        parse_json(url, &resp)
    }

    /// Authenticated PUT of a JSON body, returning the parsed JSON response.
    pub async fn put_json<B: serde::Serialize, T: DeserializeOwned>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T, GoogleError> {
        let bytes = json_body(body)?;
        let resp = self
            .send_authed(|access| json_call(json_request(Request::put(url), &bytes), access))
            .await?;
        parse_json(url, &resp)
    }

    /// Authenticated DELETE, discarding the (typically empty) response body.
    pub async fn delete_no_content(&self, url: &str) -> Result<(), GoogleError> {
        self.send_authed(|access| json_call(Request::delete(url), access))
            .await?;
        Ok(())
    }

    /// Authenticated PATCH of a JSON body, returning the parsed JSON response.
    /// (PATCH merges — unlike PUT it preserves fields you don't send.)
    pub async fn patch_json<B: serde::Serialize, T: DeserializeOwned>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T, GoogleError> {
        let bytes = json_body(body)?;
        let resp = self
            .send_authed(|access| {
                json_call(
                    json_request(Request::new(Method::Patch, url), &bytes),
                    access,
                )
            })
            .await?;
        parse_json(url, &resp)
    }

    /// Authenticated `multipart/related` upload (Drive simple upload). `metadata`
    /// is the JSON part (e.g. `{"name":…,"parents":[…]}`), `bytes` the media part
    /// tagged with `mime`. Returns the parsed created-resource JSON.
    pub async fn upload_multipart<T: DeserializeOwned>(
        &self,
        url: &str,
        metadata: &serde_json::Value,
        mime: &str,
        bytes: Vec<u8>,
    ) -> Result<T, GoogleError> {
        let meta = serde_json::to_vec(metadata).map_err(|e| GoogleError::Parse(e.to_string()))?;
        let form = rightkit_http::MultipartRelated::new()
            .part("application/json; charset=UTF-8", meta)
            .part(mime, bytes);
        let request = Request::post(url)
            .multipart_related(form)
            .map_err(|e| GoogleError::Parse(e.to_string()))?;
        let resp = self
            .send_authed(|access| bulk_call(request.clone(), access))
            .await?;
        parse_json(url, &resp)
    }

    /// Core auth wrapper: send (with transient backoff); on a 401, refresh the
    /// token once and resend. A 401 that survives the refresh becomes `Auth`.
    /// `build` produces a fresh request from an access token each call.
    async fn send_authed<F>(&self, build: F) -> Result<Response, GoogleError>
    where
        F: Fn(&str) -> Request,
    {
        let access = self.access_token().await?;
        match self.send_with_backoff(&build, &access).await {
            Err(GoogleError::Http { status: 401, .. }) => {
                self.invalidate().await?;
                let access = self.access_token().await?;
                match self.send_with_backoff(&build, &access).await {
                    Err(GoogleError::Http { status: 401, body }) => Err(GoogleError::Auth(body)),
                    other => other,
                }
            }
            other => other,
        }
    }

    /// Send one request through rightkit-http. Retries (429, 5xx, transport) follow
    /// `gmail_retry_policy`, and a 429 that outlives them keeps the server's
    /// Retry-After so the batch backoff can honor it. 401 is returned as
    /// `Http { status: 401 }` for `send_authed` to handle.
    async fn send_with_backoff<F>(&self, build: &F, access: &str) -> Result<Response, GoogleError>
    where
        F: Fn(&str) -> Request,
    {
        // POST/PATCH are not replayed on 429/5xx (rightkit-http default): a 5xx after
        // Gmail accepted a send would otherwise duplicate the message. A 429 still
        // surfaces as RateLimitedAfter for the batch backoff.
        let req = build(access).retry(gmail_retry_policy());
        let resp = http_client()
            .map_err(GoogleError::Transient)?
            .send_any(&req)
            .await
            .map_err(|e| classify_send_error(req.method, &e))?;
        if (200..300).contains(&resp.status) {
            return Ok(resp);
        }
        let retry_after = resp.header("retry-after").and_then(parse_retry_after);
        let body = resp.text();
        match resp.status {
            429 => Err(match retry_after {
                Some(retry_after) => GoogleError::RateLimitedAfter {
                    message: body,
                    retry_after,
                },
                None => GoogleError::RateLimited(body),
            }),
            status => Err(GoogleError::Http { status, body }),
        }
    }
}

/// Map a transport error to the typed failure, keeping the delivery classification of
/// rightkit-http's `HttpError`: for a non-idempotent method, a failure after the connect phase
/// may have reached Google (`Uncertain`); a connect-phase failure or a refused request sent
/// nothing (`Transient`). Idempotent methods stay `Transient` (safe to repeat).
pub fn classify_send_error(method: Method, error: &HttpError) -> GoogleError {
    let message = error.to_string();
    if method.is_idempotent() {
        return GoogleError::Transient(message);
    }
    let may_have_been_sent = match error {
        HttpError::Transport { connect_phase, .. } => !connect_phase,
        HttpError::Timeout { phase, .. } => *phase != rightkit_http::Phase::Connect,
        HttpError::Cancelled => true,
        _ => false,
    };
    if may_have_been_sent {
        GoogleError::Uncertain(message)
    } else {
        GoogleError::Transient(message)
    }
}
