//! The phone review and pause surface (rightkit#1489, PostRight ask bogusyogi/postright#443):
//! the serving side a native phone app calls over a daemon's public routes.
//!
//! It is built on `rightkit-daemon`'s pattern routes ([`Daemon::add_public_route_pattern`]) and
//! serves under `/review/api/*`:
//!
//! | Method and path | Does |
//! |---|---|
//! | `GET  /review/api/jobs` | the account's jobs awaiting approval and failed jobs |
//! | `POST /review/api/jobs/{id}/approve` | approve (`expected_revision`, `policy_revision`) |
//! | `POST /review/api/jobs/{id}/reject` | reject (`expected_revision`, optional `reason`) |
//! | `POST /review/api/jobs/{id}/resolve` | resolve a failure (`expected_revision`) |
//! | `POST /review/api/jobs/{id}/pause` | pause or resume one job (`paused`, default true) |
//! | `POST /review/api/account/pause` | pause or resume the whole account (`paused`, default true) |
//!
//! The host supplies the jobs and applies the decisions through [`ReviewHost`]; this module owns
//! the wire contract and the access rules:
//!
//! * **Tokens.** A phone presents `Authorization: Bearer <token>`. A token is issued per device
//!   and bound to exactly one account ([`TokenRegistry::issue`]); the registry stores only
//!   `HMAC-SHA256(server key, secret)`, never the secret, and the secret is shown once in
//!   [`IssuedToken`]. A presented secret is compared with `ring::hmac::verify`, which is
//!   constant-time, and an unknown token id costs the same comparison as a known one. A revoked
//!   or expired token is refused exactly like an unknown one (`401`).
//! * **Scope.** Every host call carries the token's account; a job id from another account is the
//!   host's `NotFound`. There is no account parameter on the wire.
//! * **Autonomy gating.** Approve, reject and resolve are accepted only while the job's
//!   pairing is at [`Level::Reviewed`] or [`Level::ReviewLight`]; at [`Level::Autonomous`] they
//!   are `409 autonomy_gated`. Pause works at any level.
//! * **Rate limiting.** Failed authentication is counted per remote address and globally by the
//!   daemon's [`FailLimiter`]; a blocked source gets `429` on its next bad token. A valid token
//!   is never throttled by the failure counter.
//! * **Conflicts.** A stale `expected_revision` or `policy_revision`, or a job that is not in a
//!   state the action accepts, is `409` with a stable `error` code (`revision_conflict`,
//!   `policy_revision_conflict`, `state_conflict`).
//!
//! The module never logs and never puts a token, a hash of one or a host message in a response
//! or an error. The wire is JSON; unknown request fields are refused.

use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rightkit_daemon::remote::{hex, unhex, Daemon, Request, Response};
use rightkit_daemon::FailLimiter;
use ring::hmac;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::ladder::Level;

/// The route prefix every phone route lives under.
pub const ROUTE_PREFIX: &str = "/review/api";
/// The version tag at the start of every token.
pub const TOKEN_VERSION: &str = "rkrv1";
/// The largest request body a phone route reads.
pub const BODY_LIMIT: usize = 16 * 1024;
/// The shortest server key [`TokenRegistry::open`] accepts, in bytes.
pub const MIN_KEY_BYTES: usize = 32;

const SECRET_BYTES: usize = 32;
const ID_BYTES: usize = 8;
const MAX_TOKEN_LEN: usize = 128;
const MAX_IDENT_LEN: usize = 128;
const MAX_REASON_CHARS: usize = 1000;
const MAX_LABEL_CHARS: usize = 80;

// ---- errors ------------------------------------------------------------------------------------

/// Why a token operation or the surface setup failed. Never carries a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhoneError {
    /// The server key is shorter than [`MIN_KEY_BYTES`].
    WeakKey,
    /// An account, label or id is empty, too long or has characters outside the allowed set.
    InvalidInput(&'static str),
    /// The system random source failed.
    Random,
    /// The host's persist callback refused; nothing changed.
    Persist(String),
    /// The daemon refused a route registration.
    Route(&'static str),
}

impl std::fmt::Display for PhoneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PhoneError::WeakKey => write!(
                f,
                "the token server key must be at least {MIN_KEY_BYTES} bytes"
            ),
            PhoneError::InvalidInput(what) => write!(f, "invalid input: {what}"),
            PhoneError::Random => f.write_str("the system random source failed"),
            PhoneError::Persist(why) => write!(f, "token records were not saved: {why}"),
            PhoneError::Route(why) => write!(f, "phone route not registered: {why}"),
        }
    }
}

impl std::error::Error for PhoneError {}

/// What a [`ReviewHost`] answers when it cannot do what a phone asked. The surface maps each to
/// one status and one stable code; the host's own text is never sent to the phone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    /// No such job for this account (`404`).
    NotFound,
    /// `expected_revision` is stale; `current` is the job's revision now (`409`).
    RevisionConflict { current: u64 },
    /// `policy_revision` is stale; `current` is the policy revision now (`409`).
    PolicyConflict { current: u64 },
    /// The job is not in a state that takes this action, or it is paused (`409`).
    StateConflict,
    /// The host cannot serve right now (`503`).
    Unavailable,
}

// ---- tokens ------------------------------------------------------------------------------------

/// One stored token: the HMAC of its secret, bound to an account and a device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenRecord {
    /// The public token id (16 hex characters).
    pub id: String,
    /// The one account this token may act on.
    pub account: String,
    /// A device label for the operator's list (not secret).
    pub label: String,
    /// Lower-case hex of `HMAC-SHA256(server key, secret)`.
    pub tag: String,
    /// Issue time, Unix seconds.
    pub issued_at: u64,
    /// Expiry, Unix seconds; `None` never expires.
    pub expires_at: Option<u64>,
    /// Revocation time, Unix seconds; `None` while live.
    pub revoked_at: Option<u64>,
}

/// A freshly issued token. The secret appears only here; it cannot be recovered later.
pub struct IssuedToken {
    /// The public id, for [`TokenRegistry::revoke`] and the operator's list.
    pub id: String,
    /// The bearer token to hand to the device, once.
    pub token: String,
}

impl std::fmt::Debug for IssuedToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedToken")
            .field("id", &self.id)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// A verified token: which record, and the account it is bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenGrant {
    pub id: String,
    pub account: String,
}

/// The host's durable save of every token record. Called with the full new set after each
/// change; an `Err` rolls the change back.
pub type PersistFn = Box<dyn Fn(&[TokenRecord]) -> Result<(), String> + Send + Sync>;

/// The per-device bearer tokens of one host.
pub struct TokenRegistry {
    key: hmac::Key,
    records: Mutex<BTreeMap<String, TokenRecord>>,
    persist: PersistFn,
}

impl std::fmt::Debug for TokenRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenRegistry").finish_non_exhaustive()
    }
}

impl TokenRegistry {
    /// Open a registry over `key` (the host's secret, at least [`MIN_KEY_BYTES`] of it, kept in
    /// the host's credential store and never in the records) and the `records` the host saved
    /// last. `persist` saves the whole set after each issue or revoke.
    pub fn open(
        key: &[u8],
        records: Vec<TokenRecord>,
        persist: PersistFn,
    ) -> Result<Self, PhoneError> {
        if key.len() < MIN_KEY_BYTES {
            return Err(PhoneError::WeakKey);
        }
        let mut map = BTreeMap::new();
        for record in records {
            map.insert(record.id.clone(), record);
        }
        Ok(Self {
            key: hmac::Key::new(hmac::HMAC_SHA256, key),
            records: Mutex::new(map),
            persist,
        })
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, TokenRecord>> {
        self.records.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Issue a token for `account` on the device `label`, expiring `ttl_secs` after now (or
    /// never). Returns the only copy of the secret.
    pub fn issue(
        &self,
        account: &str,
        label: &str,
        ttl_secs: Option<u64>,
    ) -> Result<IssuedToken, PhoneError> {
        if !valid_ident(account) {
            return Err(PhoneError::InvalidInput("account"));
        }
        let label = label.trim();
        if label.is_empty()
            || label.chars().count() > MAX_LABEL_CHARS
            || label.chars().any(char::is_control)
        {
            return Err(PhoneError::InvalidInput("label"));
        }
        let now = now_secs();
        let rng = SystemRandom::new();
        let mut secret = [0u8; SECRET_BYTES];
        rng.fill(&mut secret).map_err(|_| PhoneError::Random)?;
        let tag = hmac::sign(&self.key, &secret);

        let mut records = self.lock();
        let mut id = String::new();
        for _ in 0..8 {
            let mut raw = [0u8; ID_BYTES];
            rng.fill(&mut raw).map_err(|_| PhoneError::Random)?;
            let candidate = hex(&raw);
            if !records.contains_key(&candidate) {
                id = candidate;
                break;
            }
        }
        if id.is_empty() {
            return Err(PhoneError::Random);
        }
        let record = TokenRecord {
            id: id.clone(),
            account: account.to_string(),
            label: label.to_string(),
            tag: hex(tag.as_ref()),
            issued_at: now,
            expires_at: ttl_secs.map(|ttl| now.saturating_add(ttl)),
            revoked_at: None,
        };
        let mut next = records.clone();
        next.insert(id.clone(), record);
        self.save(&next)?;
        *records = next;
        Ok(IssuedToken {
            token: format!("{TOKEN_VERSION}.{id}.{}", hex(&secret)),
            id,
        })
    }

    /// Revoke one token by id. `Ok(false)` when there is no such live token.
    pub fn revoke(&self, id: &str) -> Result<bool, PhoneError> {
        let mut records = self.lock();
        let mut next = records.clone();
        match next.get_mut(id) {
            Some(record) if record.revoked_at.is_none() => record.revoked_at = Some(now_secs()),
            _ => return Ok(false),
        }
        self.save(&next)?;
        *records = next;
        Ok(true)
    }

    /// Revoke every live token of `account` (a lost phone, a removed account). Returns how many.
    pub fn revoke_account(&self, account: &str) -> Result<usize, PhoneError> {
        let mut records = self.lock();
        let mut next = records.clone();
        let now = now_secs();
        let mut revoked = 0;
        for record in next.values_mut() {
            if record.account == account && record.revoked_at.is_none() {
                record.revoked_at = Some(now);
                revoked += 1;
            }
        }
        if revoked == 0 {
            return Ok(0);
        }
        self.save(&next)?;
        *records = next;
        Ok(revoked)
    }

    /// Every record, for the operator's device list. Holds no secret.
    pub fn records(&self) -> Vec<TokenRecord> {
        self.lock().values().cloned().collect()
    }

    fn save(&self, set: &BTreeMap<String, TokenRecord>) -> Result<(), PhoneError> {
        let list: Vec<TokenRecord> = set.values().cloned().collect();
        (self.persist)(&list).map_err(PhoneError::Persist)
    }

    /// Verify a presented bearer token at `now`. `None` for anything that is not a live token
    /// of this registry: malformed, unknown id, wrong secret, revoked, expired. The secret is
    /// compared in constant time; an unknown id runs the same comparison against a zero tag so
    /// the answer's timing does not say whether the id exists.
    pub fn authenticate(&self, presented: &str, now: u64) -> Option<TokenGrant> {
        if presented.len() > MAX_TOKEN_LEN {
            return None;
        }
        let mut parts = presented.split('.');
        let (version, id, secret) = (parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() || version != TOKEN_VERSION || !valid_ident(id) {
            return None;
        }
        let secret = unhex(secret).filter(|bytes| bytes.len() == SECRET_BYTES)?;
        let record = self.lock().get(id).cloned();
        let stored = record
            .as_ref()
            .and_then(|record| unhex(&record.tag))
            .unwrap_or_else(|| vec![0u8; 32]);
        let matches = hmac::verify(&self.key, &secret, &stored).is_ok();
        let record = record?;
        if !matches
            || record.revoked_at.is_some()
            || record.expires_at.is_some_and(|exp| now >= exp)
        {
            return None;
        }
        Some(TokenGrant {
            id: record.id,
            account: record.account,
        })
    }
}

// ---- the host contract -------------------------------------------------------------------------

/// Where a job stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Waiting for an approve or reject.
    AwaitingApproval,
    /// Failed and waiting for the operator to resolve it.
    Failed,
}

/// One job as the phone sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewJob {
    /// The job id (also the path segment; see [`valid_ident`]).
    pub id: String,
    pub state: JobState,
    /// The job's revision; send it back as `expected_revision`.
    pub revision: u64,
    /// The review policy's revision; send it back as `policy_revision` on approve.
    pub policy_revision: u64,
    pub title: String,
    pub content_type: String,
    pub platform: String,
    /// The autonomy level of the job's (content type, platform) pairing.
    pub autonomy: Level,
    /// Whether the job is paused.
    pub paused: bool,
    /// What the reviewer should look at, when the host has it.
    pub summary: Option<String>,
    /// Why it failed, for [`JobState::Failed`].
    pub failure: Option<String>,
}

/// The host side of the surface: the jobs and the effects. Every call names the token's account;
/// the host must never act on or return another account's job.
pub trait ReviewHost: Send + Sync + 'static {
    /// The account's jobs awaiting approval and its failed jobs.
    fn jobs(&self, account: &str) -> Result<Vec<ReviewJob>, HostError>;
    /// The autonomy level of the job's pairing right now.
    fn autonomy(&self, account: &str, job: &str) -> Result<Level, HostError>;
    /// Approve the job at `expected_revision` under `policy_revision`.
    fn approve(
        &self,
        account: &str,
        job: &str,
        expected_revision: u64,
        policy_revision: u64,
    ) -> Result<(), HostError>;
    /// Reject the job at `expected_revision`.
    fn reject(
        &self,
        account: &str,
        job: &str,
        expected_revision: u64,
        reason: Option<&str>,
    ) -> Result<(), HostError>;
    /// Resolve a failed job at `expected_revision`.
    fn resolve(&self, account: &str, job: &str, expected_revision: u64) -> Result<(), HostError>;
    /// Pause (`true`) or resume (`false`) one job.
    fn pause_job(&self, account: &str, job: &str, paused: bool) -> Result<(), HostError>;
    /// Pause or resume every job of the account.
    fn pause_account(&self, account: &str, paused: bool) -> Result<(), HostError>;
}

// ---- the surface -------------------------------------------------------------------------------

/// The phone surface of one host.
pub struct PhoneSurface {
    host: Arc<dyn ReviewHost>,
    tokens: Arc<TokenRegistry>,
    limiter: Mutex<FailLimiter>,
}

impl PhoneSurface {
    pub fn new(host: Arc<dyn ReviewHost>, tokens: Arc<TokenRegistry>) -> Arc<Self> {
        Arc::new(Self {
            host,
            tokens,
            limiter: Mutex::new(FailLimiter::new()),
        })
    }

    /// Register `GET` and `POST` `/review/api/*` on `daemon`. The daemon's public routes carry no
    /// authentication of their own; every request is checked by [`handle`](Self::handle).
    pub fn register(self: &Arc<Self>, daemon: &Daemon) -> Result<(), PhoneError> {
        let pattern = format!("{ROUTE_PREFIX}/*");
        for method in ["GET", "POST"] {
            let surface = Arc::clone(self);
            daemon
                .add_public_route_pattern(method, &pattern, BODY_LIMIT, move |request| {
                    surface.handle(request)
                })
                .map_err(PhoneError::Route)?;
        }
        Ok(())
    }

    /// Serve one request: authenticate, then route.
    pub fn handle(&self, request: &Request) -> Response {
        let grant = match self.authenticate(request) {
            Ok(grant) => grant,
            Err(response) => return response,
        };
        let Some(rest) = request.params.get("*") else {
            return error(404, "not_found");
        };
        let segments: Vec<&str> = rest.split('/').collect();
        let method = request.method.as_str();
        match (method, segments.as_slice()) {
            ("GET", ["jobs"]) => self.list(&grant),
            (_, ["jobs"]) => error(405, "method_not_allowed"),
            ("POST", ["jobs", id, action]) => {
                if !valid_ident(id) {
                    return error(404, "not_found");
                }
                self.job_action(&grant, id, action, &request.body)
            }
            (_, ["jobs", id, "approve" | "reject" | "resolve" | "pause"]) if valid_ident(id) => {
                error(405, "method_not_allowed")
            }
            ("POST", ["account", "pause"]) => self.pause_account(&grant, &request.body),
            (_, ["account", "pause"]) => error(405, "method_not_allowed"),
            _ => error(404, "not_found"),
        }
    }

    fn limiter(&self) -> MutexGuard<'_, FailLimiter> {
        self.limiter.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn authenticate(&self, request: &Request) -> Result<TokenGrant, Response> {
        let Some(header) = request.header("authorization") else {
            return Err(unauthorized());
        };
        let presented = header
            .split_once(' ')
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .map(|(_, token)| token.trim());
        let Some(presented) = presented else {
            return Err(unauthorized());
        };
        if let Some(grant) = self.tokens.authenticate(presented, now_secs()) {
            return Ok(grant);
        }
        let source = source_key(request);
        let now = Instant::now();
        let mut limiter = self.limiter();
        if limiter.refuses_unverified(source, now) {
            return Err(error(429, "rate_limited").with_header("retry-after", "60"));
        }
        limiter.record_failure(source, now);
        Err(unauthorized())
    }

    fn list(&self, grant: &TokenGrant) -> Response {
        match self.host.jobs(&grant.account) {
            Ok(jobs) => ok(json!({ "jobs": jobs })),
            Err(failure) => host_error(failure),
        }
    }

    fn job_action(&self, grant: &TokenGrant, id: &str, action: &str, body: &[u8]) -> Response {
        let account = grant.account.as_str();
        match action {
            "approve" => {
                let Ok(input) = parse::<ApproveBody>(body) else {
                    return error(400, "invalid_request");
                };
                self.gated(account, id, |host| {
                    host.approve(account, id, input.expected_revision, input.policy_revision)
                })
            }
            "reject" => {
                let Ok(input) = parse::<RejectBody>(body) else {
                    return error(400, "invalid_request");
                };
                if input
                    .reason
                    .as_deref()
                    .is_some_and(|reason| reason.chars().count() > MAX_REASON_CHARS)
                {
                    return error(400, "invalid_request");
                }
                self.gated(account, id, |host| {
                    host.reject(
                        account,
                        id,
                        input.expected_revision,
                        input.reason.as_deref(),
                    )
                })
            }
            "resolve" => {
                let Ok(input) = parse::<ResolveBody>(body) else {
                    return error(400, "invalid_request");
                };
                self.gated(account, id, |host| {
                    host.resolve(account, id, input.expected_revision)
                })
            }
            "pause" => {
                let Ok(input) = parse_optional::<PauseBody>(body) else {
                    return error(400, "invalid_request");
                };
                done(self.host.pause_job(account, id, input.paused))
            }
            _ => error(404, "not_found"),
        }
    }

    fn pause_account(&self, grant: &TokenGrant, body: &[u8]) -> Response {
        let Ok(input) = parse_optional::<PauseBody>(body) else {
            return error(400, "invalid_request");
        };
        done(self.host.pause_account(&grant.account, input.paused))
    }

    /// Run a decision only while the job's pairing is reviewable.
    fn gated(
        &self,
        account: &str,
        id: &str,
        act: impl FnOnce(&dyn ReviewHost) -> Result<(), HostError>,
    ) -> Response {
        match self.host.autonomy(account, id) {
            Ok(Level::Reviewed | Level::ReviewLight) => done(act(self.host.as_ref())),
            Ok(Level::Autonomous) => error(409, "autonomy_gated"),
            Err(failure) => host_error(failure),
        }
    }
}

// ---- wire bodies -------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApproveBody {
    expected_revision: u64,
    policy_revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RejectBody {
    expected_revision: u64,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveBody {
    expected_revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PauseBody {
    #[serde(default = "pause_by_default")]
    paused: bool,
}

fn pause_by_default() -> bool {
    true
}

fn parse<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, serde_json::Error> {
    serde_json::from_slice(body)
}

/// As [`parse`], reading an empty body as `{}`.
fn parse_optional<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, serde_json::Error> {
    if body.iter().all(u8::is_ascii_whitespace) {
        serde_json::from_slice(b"{}")
    } else {
        serde_json::from_slice(body)
    }
}

// ---- responses ---------------------------------------------------------------------------------

fn ok(body: Value) -> Response {
    Response::json(200, body).with_header("cache-control", "no-store")
}

fn error(status: u16, code: &str) -> Response {
    Response::json(status, json!({ "error": code })).with_header("cache-control", "no-store")
}

fn unauthorized() -> Response {
    error(401, "unauthorized").with_header("www-authenticate", "Bearer")
}

fn done(result: Result<(), HostError>) -> Response {
    match result {
        Ok(()) => ok(json!({ "ok": true })),
        Err(failure) => host_error(failure),
    }
}

fn host_error(failure: HostError) -> Response {
    let response = match failure {
        HostError::NotFound => error(404, "not_found"),
        HostError::RevisionConflict { current } => Response::json(
            409,
            json!({ "error": "revision_conflict", "current_revision": current }),
        ),
        HostError::PolicyConflict { current } => Response::json(
            409,
            json!({ "error": "policy_revision_conflict", "current_policy_revision": current }),
        ),
        HostError::StateConflict => error(409, "state_conflict"),
        HostError::Unavailable => error(503, "unavailable"),
    };
    response.with_header("cache-control", "no-store")
}

// ---- helpers -----------------------------------------------------------------------------------

/// Whether `value` is a legal account, job or token id: 1 to 128 characters of ASCII letters,
/// digits and `_ - . : @`, and not `.` or `..`.
pub fn valid_ident(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IDENT_LEN
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':' | b'@'))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// The failure counter's key: the remote IP address (not the port, which changes per
/// connection). A request with no peer address shares one bucket.
fn source_key(request: &Request) -> u64 {
    let Some(peer) = request.peer else {
        return 0;
    };
    let mut hasher = DefaultHasher::new();
    match peer.ip() {
        IpAddr::V4(ip) => ip.octets().hash(&mut hasher),
        IpAddr::V6(ip) => ip.octets().hash(&mut hasher),
    }
    hasher.finish()
}
