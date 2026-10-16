//! Entitlement source keyed to the Google subject (feature `subject-entitlement`).
//!
//! The shared entitlement service (`bogusyogi/rightsites#81`) answers one question for a
//! signed-in Google account: does this `sub` hold Pro for this app, whether it was bought
//! through Play Billing, through a Windows licence activation, or through a StoreKit purchase
//! linked to the account. This module is a client for that service. It is **additive**: the
//! offline Ed25519 token path ([`crate::verify_token`], [`crate::EngineEntitlements`], the
//! lifecycle and refresh modules) and any StoreKit handling are untouched and behave exactly
//! as before when the feature is off or unused.
//!
//! ## Trust model
//! The answer is plain JSON over TLS, authenticated by the service, not an offline-verifiable
//! signed token. A persisted cache is local data and is not tamper-proof. Use this source where
//! the Google account is the entitlement key (Android, and cross-platform Pro); keep the signed
//! token path where offline cryptographic enforcement is required.
//!
//! ## Wire contract (rightsites#81)
//! Every call carries `Authorization: Bearer <Google ID token>`; the token comes from the
//! caller's [`BearerTokenProvider`] (this crate does not depend on `rightkit-oauth`).
//!
//! | Call | Request | Response |
//! |---|---|---|
//! | `GET /v1/entitlement?app=<app_id>` | none | entitlement object |
//! | `POST /v1/licenses/bridge/from-play` | `{"app","package_name","product_id","purchase_token"}` | entitlement object |
//! | `POST /v1/licenses/bridge/from-storekit` | `{"app","jws"}` (assumed, see below) | entitlement object |
//! | `POST /v1/licenses/bridge/from-activation` | `{"app","activation_token","device_id"}` (assumed) | entitlement object |
//!
//! The entitlement object is
//! `{"app","pro":bool,"source":"play|storekit|license|none","expires_at":"<RFC3339>|null","checked_at":"<RFC3339>"}`.
//!
//! rightsites#81 pins the from-play body and every response, and calls the from-storekit and
//! from-activation endpoints "existing" without giving their request bodies. The two bodies
//! above are the minimal definitions this module assumes; confirm them against the deployed
//! service before wiring those two calls.
//!
//! ## Caching, expiry and offline grace
//! - A successful answer is cached with the local time it was fetched.
//! - It is served from cache until `recheck_interval_secs` (default and maximum 24 hours, the
//!   contract's "re-check at least daily") has passed, then the service is asked again.
//! - `expires_at` is honoured strictly: once the local clock reaches it the entitlement no
//!   longer grants Pro and a re-check is forced, even inside the re-check interval. Offline
//!   grace never extends an `expires_at`.
//! - **Offline grace is 7 days** (`offline_grace_secs`, default and maximum 604800), measured
//!   from the last successful check on the local clock. If a re-check fails for a transient
//!   reason (no network, timeout, 5xx, 429, unreadable answer) and the cached answer is
//!   younger than the grace window, the cached answer is returned marked
//!   [`Freshness::OfflineGrace`]. Past the window the transient error is returned.
//! - A 401 (after one forced token refresh and one retry) or a 403 means the account is not
//!   authorised: the cache is cleared and [`SubjectEntitlementError::Unauthorized`] is returned
//!   with no grace. Fail closed.
//! - If the local clock reads earlier than the cached fetch time the cache is not trusted.
//!
//! This crate must not spawn threads or sleep: there is no background refresh. The caller
//! invokes [`SubjectEntitlementSource::entitlement`] when it needs a decision.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rightkit_http::{percent_encode, Client, ClientConfig, ErrorClass, Method, Request};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::system_now_unix;

/// Re-check interval ceiling and default: 24 hours.
pub const SUBJECT_RECHECK_SECS: i64 = 24 * 60 * 60;
/// Offline grace ceiling and default: 7 days.
pub const SUBJECT_OFFLINE_GRACE_SECS: i64 = 7 * 24 * 60 * 60;

const CACHE_RECORD_VERSION: u8 = 1;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// Supplies the bearer token for the entitlement service (a Google ID token today).
///
/// `force_refresh` is `false` on the first attempt. If the service answers 401 the source calls
/// again with `true`, once, and the provider must then return a freshly minted token rather than
/// a cached one. The returned token is never logged or stored by this crate.
pub trait BearerTokenProvider: Send + Sync {
    fn bearer_token(&self, force_refresh: bool) -> Result<String, TokenError>;
}

impl<F> BearerTokenProvider for F
where
    F: Fn(bool) -> Result<String, TokenError> + Send + Sync,
{
    fn bearer_token(&self, force_refresh: bool) -> Result<String, TokenError> {
        self(force_refresh)
    }
}

/// The provider could not produce a token (signed out, consent revoked, no network to the
/// identity provider). Carries a message only; never put the token in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenError(pub String);

impl TokenError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for TokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for TokenError {}

/// Where the cached answer is persisted between runs (needed for offline grace to survive a
/// restart). The store must be scoped to the signed-in account; call
/// [`SubjectEntitlementSource::invalidate`] on sign-out or account switch. Failures are
/// swallowed: a store that cannot persist only costs the cross-restart grace.
pub trait SubjectCacheStore: Send + Sync {
    fn load(&self) -> Option<String>;
    fn save(&self, value: &str);
    fn clear(&self);
}

impl<S: SubjectCacheStore + ?Sized> SubjectCacheStore for Arc<S> {
    fn load(&self) -> Option<String> {
        (**self).load()
    }
    fn save(&self, value: &str) {
        (**self).save(value)
    }
    fn clear(&self) {
        (**self).clear()
    }
}

/// In-memory [`SubjectCacheStore`] (the default). Lost on restart.
#[derive(Debug, Default)]
pub struct MemorySubjectCache(Mutex<Option<String>>);

impl MemorySubjectCache {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SubjectCacheStore for MemorySubjectCache {
    fn load(&self) -> Option<String> {
        lock(&self.0).clone()
    }
    fn save(&self, value: &str) {
        *lock(&self.0) = Some(value.to_string());
    }
    fn clear(&self) {
        *lock(&self.0) = None;
    }
}

/// Which purchase channel granted the entitlement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubjectSource {
    #[serde(rename = "play")]
    Play,
    #[serde(rename = "storekit")]
    Storekit,
    #[serde(rename = "license")]
    License,
    #[serde(rename = "none")]
    Absent,
    /// A source string this version does not know.
    #[serde(other)]
    Unknown,
}

/// The service's entitlement object with times as unix seconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectEntitlement {
    pub app: String,
    pub pro: bool,
    pub source: SubjectSource,
    /// `None` means no expiry (for example a lifetime Windows licence).
    pub expires_at: Option<i64>,
    /// Server time of the check.
    pub checked_at: i64,
}

impl SubjectEntitlement {
    /// True when `expires_at` has been reached at `now`.
    pub fn is_lapsed(&self, now: i64) -> bool {
        matches!(self.expires_at, Some(expires) if now >= expires)
    }

    /// Pro right now: the service said Pro and `expires_at` has not been reached.
    pub fn is_pro_at(&self, now: i64) -> bool {
        self.pro && !self.is_lapsed(now)
    }
}

/// How current a returned answer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Fetched from the service by this call.
    Fresh,
    /// Served from cache inside the re-check interval.
    Cached,
    /// The service could not be reached for a re-check; served from cache inside the offline
    /// grace window, which ends at `grace_ends_at` (unix seconds, local clock).
    OfflineGrace { grace_ends_at: i64 },
}

/// An answer plus the verdict at the time it was produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubjectDecision {
    pub entitlement: SubjectEntitlement,
    pub freshness: Freshness,
    /// `entitlement.pro` and not past `expires_at` on the local clock at decision time.
    pub pro: bool,
}

impl SubjectDecision {
    pub fn is_pro(&self) -> bool {
        self.pro
    }
}

/// Everything that can go wrong. All variants fail closed: no variant grants Pro.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SubjectEntitlementError {
    /// The [`BearerTokenProvider`] failed; no request was sent.
    Token(TokenError),
    /// 401 after a forced token refresh, or 403. The cache was cleared.
    Unauthorized,
    /// Could not reach the service (connect, TLS, timeout). Transient.
    Offline(String),
    /// HTTP 429. Transient.
    RateLimited { retry_after: Option<Duration> },
    /// HTTP 5xx. Transient.
    Server { status: u16 },
    /// The answer was not a usable entitlement object. Transient (a proxy page or a bad
    /// deploy must not wipe a paying user's grace).
    BadResponse(String),
    /// Any other 4xx (a rejected purchase token, an unknown app). Not transient. `message`
    /// is the HTTP status only; response bodies are never copied into errors.
    Rejected { status: u16 },
    /// The request could not be made as configured (bad base URL, address policy, HTTPS
    /// required). Not transient.
    Config(String),
}

impl SubjectEntitlementError {
    /// True when offline grace may cover this failure.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::Offline(_)
                | Self::RateLimited { .. }
                | Self::Server { .. }
                | Self::BadResponse(_)
        )
    }
}

impl fmt::Display for SubjectEntitlementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Token(e) => write!(f, "no bearer token available: {e}"),
            Self::Unauthorized => f.write_str("entitlement service rejected the account token"),
            Self::Offline(m) => write!(f, "entitlement service unreachable: {m}"),
            Self::RateLimited { .. } => f.write_str("entitlement service rate-limited the request"),
            Self::Server { status } => write!(f, "entitlement service error (HTTP {status})"),
            Self::BadResponse(m) => write!(f, "entitlement response unusable: {m}"),
            Self::Rejected { status } => write!(f, "entitlement service refused (HTTP {status})"),
            Self::Config(m) => write!(f, "entitlement request not sent: {m}"),
        }
    }
}
impl std::error::Error for SubjectEntitlementError {}

impl From<TokenError> for SubjectEntitlementError {
    fn from(e: TokenError) -> Self {
        Self::Token(e)
    }
}

/// Configuration. `base_url` has no trailing path, for example `https://entitlements.example`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubjectEntitlementConfig {
    pub base_url: String,
    pub app: String,
    /// Seconds a cached answer is served before a re-check. Clamped to `0..=86400`.
    pub recheck_interval_secs: i64,
    /// Offline grace in seconds. Clamped to `0..=604800`.
    pub offline_grace_secs: i64,
}

impl SubjectEntitlementConfig {
    pub fn new(base_url: impl Into<String>, app: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            app: app.into(),
            recheck_interval_secs: SUBJECT_RECHECK_SECS,
            offline_grace_secs: SUBJECT_OFFLINE_GRACE_SECS,
        }
    }

    fn recheck(&self) -> i64 {
        self.recheck_interval_secs.clamp(0, SUBJECT_RECHECK_SECS)
    }

    fn grace(&self) -> i64 {
        self.offline_grace_secs.clamp(0, SUBJECT_OFFLINE_GRACE_SECS)
    }
}

/// A Play Billing purchase to link to the signed-in account.
#[derive(Clone, PartialEq, Eq)]
pub struct PlayPurchase {
    pub package_name: String,
    pub product_id: String,
    pub purchase_token: String,
}

impl fmt::Debug for PlayPurchase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlayPurchase")
            .field("package_name", &self.package_name)
            .field("product_id", &self.product_id)
            .field("purchase_token", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheRecord {
    v: u8,
    entitlement: SubjectEntitlement,
    /// Local unix time of the successful check.
    fetched_at: i64,
}

#[derive(Default)]
struct State {
    loaded: bool,
    record: Option<CacheRecord>,
}

#[derive(Deserialize)]
struct WireEntitlement {
    app: String,
    pro: bool,
    source: SubjectSource,
    #[serde(default)]
    expires_at: Option<String>,
    checked_at: String,
}

type Clock = Box<dyn Fn() -> i64 + Send + Sync>;

/// Entitlement source backed by the shared entitlement service, keyed to the Google subject.
pub struct SubjectEntitlementSource {
    config: SubjectEntitlementConfig,
    client: Client,
    tokens: Box<dyn BearerTokenProvider>,
    store: Box<dyn SubjectCacheStore>,
    now: Clock,
    state: Mutex<State>,
}

impl SubjectEntitlementSource {
    /// Default client: HTTPS only, public addresses only, platform trust roots.
    pub fn new(
        config: SubjectEntitlementConfig,
        tokens: impl BearerTokenProvider + 'static,
    ) -> Self {
        Self {
            config,
            client: Client::new(ClientConfig::default().https_only(true)),
            tokens: Box::new(tokens),
            store: Box::new(MemorySubjectCache::new()),
            now: Box::new(|| system_now_unix().unwrap_or(0)),
            state: Mutex::new(State::default()),
        }
    }

    /// Use a caller-built HTTP client (proxy, trust roots, test policy).
    pub fn with_client(mut self, client: Client) -> Self {
        self.client = client;
        self
    }

    /// Persist the cache (so offline grace survives a restart).
    pub fn with_store(mut self, store: impl SubjectCacheStore + 'static) -> Self {
        self.store = Box::new(store);
        self.state = Mutex::new(State::default());
        self
    }

    /// Replace the wall clock (unix seconds). Intended for tests and for hosts that already
    /// hold a trusted time.
    pub fn with_clock(mut self, now: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        self.now = Box::new(now);
        self
    }

    /// The current decision: from cache when it is young enough, otherwise from the service,
    /// otherwise from the cache inside the offline grace window. See the module docs.
    pub fn entitlement(&self) -> Result<SubjectDecision, SubjectEntitlementError> {
        let now = (self.now)();
        let cached = self.cached();
        if let Some(record) = &cached {
            if self.usable(record, now)
                && now - record.fetched_at < self.config.recheck()
                && !record.entitlement.is_lapsed(now)
            {
                return Ok(decide(record, Freshness::Cached, now));
            }
        }
        self.check(cached, now)
    }

    /// Ask the service now, ignoring the re-check interval. Offline grace still applies on a
    /// transient failure.
    pub fn refresh(&self) -> Result<SubjectDecision, SubjectEntitlementError> {
        let now = (self.now)();
        let cached = self.cached();
        self.check(cached, now)
    }

    /// The decision the local cache alone supports, with no token request and no network.
    ///
    /// `Cached` inside the re-check interval, `OfflineGrace` past it but inside the offline
    /// grace window, `None` when there is no usable cache (none stored, another app, a clock
    /// earlier than the fetch time, or past the grace window). A lapsed answer is returned with
    /// `pro == false`; it never grants. This is the read a headless engine uses when it has no
    /// way to obtain a bearer token.
    pub fn cached_decision(&self) -> Option<SubjectDecision> {
        let now = (self.now)();
        let record = self.cached()?;
        if !self.usable(&record, now) {
            return None;
        }
        let age = now - record.fetched_at;
        let grace_ends_at = record.fetched_at.saturating_add(self.config.grace());
        if age < self.config.recheck() && !record.entitlement.is_lapsed(now) {
            Some(decide(&record, Freshness::Cached, now))
        } else if now < grace_ends_at {
            Some(decide(
                &record,
                Freshness::OfflineGrace { grace_ends_at },
                now,
            ))
        } else {
            None
        }
    }

    /// `true` only when a decision is available and grants Pro. Any error is `false`.
    pub fn is_pro(&self) -> bool {
        self.entitlement().map(|d| d.is_pro()).unwrap_or(false)
    }

    /// Forget the cached answer (sign-out, account switch).
    pub fn invalidate(&self) {
        let mut state = lock(&self.state);
        state.loaded = true;
        state.record = None;
        drop(state);
        self.store.clear();
    }

    /// Link a Play Billing purchase to the account. The service verifies and acknowledges it
    /// and returns the entitlement, which is cached.
    pub fn link_play_purchase(
        &self,
        purchase: &PlayPurchase,
    ) -> Result<SubjectDecision, SubjectEntitlementError> {
        self.bridge(
            "/v1/licenses/bridge/from-play",
            json!({
                "app": self.config.app,
                "package_name": purchase.package_name,
                "product_id": purchase.product_id,
                "purchase_token": purchase.purchase_token,
            }),
        )
    }

    /// Link a StoreKit purchase (JWS receipt) to the account. Request body assumed, see the
    /// module docs.
    pub fn link_storekit(&self, jws: &str) -> Result<SubjectDecision, SubjectEntitlementError> {
        self.bridge(
            "/v1/licenses/bridge/from-storekit",
            json!({ "app": self.config.app, "jws": jws }),
        )
    }

    /// Link a Windows licence activation to the account. Request body assumed, see the module
    /// docs.
    pub fn link_activation(
        &self,
        activation_token: &str,
        device_id: &str,
    ) -> Result<SubjectDecision, SubjectEntitlementError> {
        self.bridge(
            "/v1/licenses/bridge/from-activation",
            json!({
                "app": self.config.app,
                "activation_token": activation_token,
                "device_id": device_id,
            }),
        )
    }

    fn bridge(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> Result<SubjectDecision, SubjectEntitlementError> {
        let url = format!("{}{}", self.base(), path);
        match self.call(Method::Post, &url, Some(&body)) {
            Ok(entitlement) => {
                let now = (self.now)();
                Ok(self.accept(entitlement, now))
            }
            Err(e) => {
                if e == SubjectEntitlementError::Unauthorized {
                    self.invalidate();
                }
                Err(e)
            }
        }
    }

    fn check(
        &self,
        cached: Option<CacheRecord>,
        now: i64,
    ) -> Result<SubjectDecision, SubjectEntitlementError> {
        let url = format!(
            "{}/v1/entitlement?app={}",
            self.base(),
            percent_encode(&self.config.app)
        );
        match self.call(Method::Get, &url, None) {
            Ok(entitlement) => Ok(self.accept(entitlement, now)),
            Err(e) if e.is_transient() => match cached {
                Some(record) if self.usable(&record, now) => {
                    let grace_ends_at = record.fetched_at.saturating_add(self.config.grace());
                    if now < grace_ends_at {
                        Ok(decide(
                            &record,
                            Freshness::OfflineGrace { grace_ends_at },
                            now,
                        ))
                    } else {
                        Err(e)
                    }
                }
                _ => Err(e),
            },
            Err(e) => {
                if e == SubjectEntitlementError::Unauthorized {
                    self.invalidate();
                }
                Err(e)
            }
        }
    }

    /// A cached record is usable for this app and a clock that has not gone backwards.
    fn usable(&self, record: &CacheRecord, now: i64) -> bool {
        record.v == CACHE_RECORD_VERSION
            && record.entitlement.app == self.config.app
            && now >= record.fetched_at
    }

    fn accept(&self, entitlement: SubjectEntitlement, now: i64) -> SubjectDecision {
        let record = CacheRecord {
            v: CACHE_RECORD_VERSION,
            entitlement,
            fetched_at: now,
        };
        if let Ok(text) = serde_json::to_string(&record) {
            self.store.save(&text);
        }
        let decision = decide(&record, Freshness::Fresh, now);
        let mut state = lock(&self.state);
        state.loaded = true;
        state.record = Some(record);
        decision
    }

    fn cached(&self) -> Option<CacheRecord> {
        let mut state = lock(&self.state);
        if !state.loaded {
            state.loaded = true;
            state.record = self
                .store
                .load()
                .and_then(|text| serde_json::from_str::<CacheRecord>(&text).ok())
                .filter(|r| r.v == CACHE_RECORD_VERSION && r.entitlement.app == self.config.app);
        }
        state.record.clone()
    }

    fn base(&self) -> &str {
        self.config.base_url.trim_end_matches('/')
    }

    /// One service call with the 401 policy: refresh the token once, retry once.
    fn call(
        &self,
        method: Method,
        url: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<SubjectEntitlement, SubjectEntitlementError> {
        let mut force_refresh = false;
        loop {
            let token = self.tokens.bearer_token(force_refresh)?;
            let mut request = Request::new(method, url)
                .header("Accept", "application/json")
                .bearer(&token)
                .max_response_bytes(MAX_RESPONSE_BYTES);
            if let Some(body) = body {
                request = request
                    .json(body)
                    .map_err(|e| SubjectEntitlementError::Config(e.to_string()))?;
            }
            let response = self
                .client
                .send_any(&request)
                .map_err(map_transport_error)?;
            match response.status {
                200..=299 => {
                    let wire: WireEntitlement = response
                        .json()
                        .map_err(|e| SubjectEntitlementError::BadResponse(e.to_string()))?;
                    return self.convert(wire);
                }
                401 if !force_refresh => {
                    force_refresh = true;
                }
                401 | 403 => return Err(SubjectEntitlementError::Unauthorized),
                429 => {
                    let retry_after = response
                        .header("retry-after")
                        .and_then(rightkit_http::parse_retry_after);
                    return Err(SubjectEntitlementError::RateLimited { retry_after });
                }
                500..=599 => {
                    return Err(SubjectEntitlementError::Server {
                        status: response.status,
                    })
                }
                status => return Err(SubjectEntitlementError::Rejected { status }),
            }
        }
    }

    fn convert(
        &self,
        wire: WireEntitlement,
    ) -> Result<SubjectEntitlement, SubjectEntitlementError> {
        if wire.app != self.config.app {
            return Err(SubjectEntitlementError::BadResponse(
                "response is for a different app".to_string(),
            ));
        }
        let checked_at = parse_rfc3339(&wire.checked_at).ok_or_else(|| {
            SubjectEntitlementError::BadResponse("checked_at is not RFC 3339".to_string())
        })?;
        let expires_at = match wire.expires_at.as_deref() {
            None => None,
            Some(text) => Some(parse_rfc3339(text).ok_or_else(|| {
                SubjectEntitlementError::BadResponse("expires_at is not RFC 3339".to_string())
            })?),
        };
        Ok(SubjectEntitlement {
            app: wire.app,
            pro: wire.pro,
            source: wire.source,
            expires_at,
            checked_at,
        })
    }
}

impl fmt::Debug for SubjectEntitlementSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubjectEntitlementSource")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

fn decide(record: &CacheRecord, freshness: Freshness, now: i64) -> SubjectDecision {
    SubjectDecision {
        entitlement: record.entitlement.clone(),
        freshness,
        pro: record.entitlement.is_pro_at(now),
    }
}

fn map_transport_error(error: rightkit_http::HttpError) -> SubjectEntitlementError {
    match error.class() {
        ErrorClass::Transport | ErrorClass::Timeout | ErrorClass::Cancelled => {
            SubjectEntitlementError::Offline(error.to_string())
        }
        _ => SubjectEntitlementError::Config(error.to_string()),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// RFC 3339 `date-time` to unix seconds. Accepts `Z` or a numeric offset and optional
/// fractional seconds (ignored). Anything else is `None`.
fn parse_rfc3339(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    if b.len() < 20 {
        return None;
    }
    let digits = |from: usize, len: usize| -> Option<i64> {
        let part = b.get(from..from + len)?;
        if !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(part).ok()?.parse().ok()
    };
    if b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let (year, month, day) = (digits(0, 4)?, digits(5, 2)?, digits(8, 2)?);
    let (hour, minute, second) = (digits(11, 2)?, digits(14, 2)?, digits(17, 2)?);
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let month_days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if day < 1 || day > month_days || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let mut at = 19;
    if b[at] == b'.' {
        at += 1;
        let start = at;
        while at < b.len() && b[at].is_ascii_digit() {
            at += 1;
        }
        if at == start {
            return None;
        }
    }
    let offset = match b.get(at)? {
        b'Z' | b'z' if at + 1 == b.len() => 0,
        sign @ (b'+' | b'-') if at + 6 == b.len() && b[at + 3] == b':' => {
            let (oh, om) = (digits(at + 1, 2)?, digits(at + 4, 2)?);
            if oh > 23 || om > 59 {
                return None;
            }
            let seconds = oh * 3600 + om * 60;
            if *sign == b'+' {
                seconds
            } else {
                -seconds
            }
        }
        _ => return None,
    };
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_known_values() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2000-03-01T00:00:00Z"), Some(951_868_800));
        assert_eq!(parse_rfc3339("2024-02-29T12:00:00Z"), Some(1_709_208_000));
        assert_eq!(
            parse_rfc3339("2026-10-16T00:00:00.123Z"),
            Some(1_792_108_800)
        );
        assert_eq!(
            parse_rfc3339("2026-10-16T02:00:00+02:00"),
            parse_rfc3339("2026-10-16T00:00:00Z")
        );
        assert_eq!(
            parse_rfc3339("2026-10-15T20:00:00-04:00"),
            parse_rfc3339("2026-10-16T00:00:00Z")
        );
        assert_eq!(parse_rfc3339("1969-12-31T23:59:59Z"), Some(-1));
    }

    #[test]
    fn rfc3339_rejects_malformed() {
        for bad in [
            "",
            "2026-10-16",
            "2026-10-16T00:00:00",
            "2026-13-16T00:00:00Z",
            "2026-02-29T00:00:00Z",
            "2026-10-16T24:00:00Z",
            "2026-10-16T00:00:00.Z",
            "2026-10-16T00:00:00+0200",
            "2026-10-16T00:00:00Zjunk",
            "+026-10-16T00:00:00Z",
        ] {
            assert_eq!(parse_rfc3339(bad), None, "{bad}");
        }
    }
}
