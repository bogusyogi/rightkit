//! Approval workflow mechanics: a request is bound to one effect, decided by a verified
//! principal, and claimed for execution exactly once (PK07-4).
//!
//! This module owns the **state machine and its guarantees**, not the wire. What the owner signs
//! (Pulse's P-256 `pulse.approval.v1` lines, Mail's account-scoped database decisions) and what the
//! effect does (send, delete, unlock) stay declared inputs: an [`ApprovalVerifier`] the app
//! implements, an [`ApprovalProfile`] of per-action data, an [`AuditSink`] and the effect itself.
//! The coordinator never re-encodes signed bytes: [`VerifyInput`] hands the verifier the stored request (id,
//! action, effect digest, account, client, expiry, revision) and the decision being made, and the
//! verifier builds its own signing input from them, as Pulse builds its lines from "the engine's
//! own record of the waiting request, never from the caller".
//! Real signing-vector compatibility is verified by the app's profile integration.
//!
//! ```text
//!   request -> Pending --decide(Approve)--> Approved --claim--> Claimed --settle--> Settled
//!                |  \--decide(Deny)--> Denied     |                |
//!                |                               |                +-- crash or timeout --> Settled(OutcomeUnknown)
//!                +--decide(Revoke) / revoke_account (Pending or Approved) --> Revoked
//!                +--cancel (Pending or Approved)  --> Cancelled
//!                +--expiry (Pending or Approved, checked on every touch) --> Expired
//! ```
//!
//! # Guarantees
//!
//! * **Request binding.** A decision applies to the stored request at the revision the approver
//!   saw ([`ApprovalCoordinator::decide`] takes it, the verifier receives it). A signature for
//!   one request, action, account or revision cannot answer another; a stale revision is
//!   [`ApprovalError::StaleRevision`]. A nonce the verifier reports is accepted once, across all
//!   requests ([`ApprovalStore::claim_nonce`]).
//! * **Scope.** The verified principal's account claims must cover the request's account (an
//!   account-restricted principal never decides an account-less request), and when the request
//!   names approvers the principal must be one of them.
//! * **Expiry and revocation.** Every touch of an open request checks the clock first; a decision
//!   or claim at or after `expires_ms` finds the request `Expired`. A revoked, denied, cancelled
//!   or expired request can never be claimed.
//! * **One execution claim.** [`ApprovalCoordinator::claim`] is a compare-and-set on the
//!   revision; of any number of concurrent claimants exactly one gets a [`ClaimToken`]. The token
//!   is not `Clone`; [`settle`](ApprovalCoordinator::settle) accepts only the token whose claim
//!   id is on record.
//! * **Audit before effect.** When the action's profile says [`AuditRequirement::BeforeEffect`]
//!   every mutating request or state transition appends a durable audit row before insert or
//!   compare-and-set. Failed appends leave approval state unchanged. Rows record attempted
//!   transitions: a later store conflict can prevent their candidate revision from committing.
//!   A coordinator without an [`AuditSink`] refuses such a request.
//! * **No exactly-once claim for external effects.** A claim that is never settled (a crash, a
//!   handler that disappeared) is resolved by [`ApprovalCoordinator::recover_claims`]: an
//!   action declared `reconcilable` asks the app's [`EffectReconciler`]; everything else becomes
//!   `Settled` with [`EffectOutcome::OutcomeUnknown`]. The coordinator never guesses that an
//!   effect did not happen.
//!
//! # Storage
//!
//! [`ApprovalStore`] is the durable half. Its contract: `replace` is an atomic compare-and-set on
//! `revision`, durable when it returns `Ok`, and `claim_nonce` is atomic. [`MemoryApprovalStore`]
//! is the in-process reference (a restart forgets everything, as Pulse's engine does today); an
//! app with a database implements the trait over a table with a unique key on `id`, a
//! `WHERE revision = ?` update, and a unique key on the nonce.
//!
//! Strings that reach a record or an audit row ([`NewRequest::detail`], outcome details,
//! reasons) go through [`redact_detail`]: anything but `[A-Za-z0-9_=. -]` withholds the whole
//! detail, so a caller cannot log message content through the approval ledger.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Longest request id, label or digest field accepted, in bytes.
const MAX_LABEL: usize = 256;
/// Longest detail kept after redaction, in bytes.
const MAX_DETAIL: usize = 200;
/// Most nonces [`MemoryApprovalStore`] remembers before it refuses new ones (fail closed).
const MAX_NONCES: usize = 4096;
/// Default bound on open (Pending or Approved) requests.
pub const DEFAULT_MAX_OPEN: usize = 1024;

// ---------------------------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------------------------

/// Milliseconds since the Unix epoch. Injected so expiry is testable.
pub trait ApprovalClock: Send + Sync {
    fn now_ms(&self) -> u64;
}

/// The system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemApprovalClock;

impl ApprovalClock for SystemApprovalClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------------------------
// Profile (data)
// ---------------------------------------------------------------------------------------------

/// Whether an audit row must be durable before each approval mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditRequirement {
    /// Rows are written when a sink exists; a failed append is counted
    /// ([`ApprovalCoordinator::audit_failures`]) and changes nothing.
    BestEffort,
    /// Every request or state transition must append its row before insert or compare-and-set.
    BeforeEffect,
}

/// Per-action policy, as data. Mail's send, forward, delete and attach and Pulse's approve and
/// unlock are entries of an [`ApprovalProfile`], not code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionProfile {
    /// Lifetime of a request that names none.
    pub default_ttl_ms: u64,
    /// Longest lifetime a request may ask for.
    pub max_ttl_ms: u64,
    pub audit: AuditRequirement,
    /// A request for this action must name an account.
    pub requires_account: bool,
    /// A request for this action must name a client (Pulse's `client:` binding).
    pub requires_client: bool,
    /// The effect can be asked whether it happened ([`EffectReconciler`]), so an unsettled claim
    /// need not become `OutcomeUnknown`.
    pub reconcilable: bool,
}

impl ActionProfile {
    /// A profile with `ttl_ms` as both default and maximum, best-effort audit, no required
    /// account or client, not reconcilable.
    pub fn new(ttl_ms: u64) -> Self {
        Self {
            default_ttl_ms: ttl_ms,
            max_ttl_ms: ttl_ms,
            audit: AuditRequirement::BestEffort,
            requires_account: false,
            requires_client: false,
            reconcilable: false,
        }
    }

    pub fn max_ttl_ms(mut self, ms: u64) -> Self {
        self.max_ttl_ms = ms;
        self
    }

    pub fn audit(mut self, audit: AuditRequirement) -> Self {
        self.audit = audit;
        self
    }

    pub fn requires_account(mut self, on: bool) -> Self {
        self.requires_account = on;
        self
    }

    pub fn requires_client(mut self, on: bool) -> Self {
        self.requires_client = on;
        self
    }

    pub fn reconcilable(mut self, on: bool) -> Self {
        self.reconcilable = on;
        self
    }
}

/// The declared actions and bounds of one app's approval workflow. Serde-able so a profile can
/// ship as a data file; [`ApprovalCoordinator::new`] validates it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalProfile {
    actions: BTreeMap<String, ActionProfile>,
    max_open: usize,
}

impl Default for ApprovalProfile {
    fn default() -> Self {
        Self {
            actions: BTreeMap::new(),
            max_open: DEFAULT_MAX_OPEN,
        }
    }
}

fn valid_label(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_LABEL
        && text.chars().all(|c| !c.is_control() && !c.is_whitespace())
}

impl ApprovalProfile {
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare an action, refusing a bad name or a policy that cannot work (a zero lifetime, a
    /// default above the maximum).
    pub fn try_action(
        mut self,
        name: impl Into<String>,
        profile: ActionProfile,
    ) -> Result<Self, ApprovalError> {
        let name = name.into();
        check_action(&name, &profile)?;
        self.actions.insert(name, profile);
        Ok(self)
    }

    /// [`try_action`](Self::try_action) for a profile built in code: a bad entry is a programming
    /// error and panics at construction.
    #[track_caller]
    pub fn action(self, name: impl Into<String>, profile: ActionProfile) -> Self {
        match self.try_action(name, profile) {
            Ok(p) => p,
            Err(e) => panic!("ApprovalProfile::action: {e}"),
        }
    }

    /// Most requests that may be open (Pending or Approved) at once; further requests are
    /// [`ApprovalError::TooManyOpen`] after expired ones are swept. Default
    /// [`DEFAULT_MAX_OPEN`].
    pub fn max_open(mut self, max: usize) -> Self {
        self.max_open = max;
        self
    }

    pub fn get(&self, action: &str) -> Option<&ActionProfile> {
        self.actions.get(action)
    }

    fn validate(&self) -> Result<(), ApprovalError> {
        if self.max_open == 0 {
            return Err(ApprovalError::Invalid("max_open must be at least 1"));
        }
        for (name, profile) in &self.actions {
            check_action(name, profile)?;
        }
        Ok(())
    }
}

fn check_action(name: &str, profile: &ActionProfile) -> Result<(), ApprovalError> {
    if !valid_label(name) {
        return Err(ApprovalError::Invalid("action name is not a plain label"));
    }
    if profile.default_ttl_ms == 0
        || profile.max_ttl_ms == 0
        || profile.default_ttl_ms > profile.max_ttl_ms
    {
        return Err(ApprovalError::Invalid(
            "action lifetime must be positive and the default no greater than the maximum",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------------------------

/// Where a request is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ApprovalState {
    Pending,
    Approved,
    Denied,
    Revoked,
    Cancelled,
    Expired,
    /// Claimed for execution; the effect may be running or may have run.
    Claimed,
    /// Done; see [`ApprovalRecord::outcome`].
    Settled,
}

impl ApprovalState {
    /// Pending or Approved: the states a request can still be decided, revoked, cancelled or
    /// expired from.
    pub fn is_open(self) -> bool {
        matches!(self, ApprovalState::Pending | ApprovalState::Approved)
    }

    /// No transition leaves this state.
    pub fn is_terminal(self) -> bool {
        !matches!(
            self,
            ApprovalState::Pending | ApprovalState::Approved | ApprovalState::Claimed
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalState::Pending => "pending",
            ApprovalState::Approved => "approved",
            ApprovalState::Denied => "denied",
            ApprovalState::Revoked => "revoked",
            ApprovalState::Cancelled => "cancelled",
            ApprovalState::Expired => "expired",
            ApprovalState::Claimed => "claimed",
            ApprovalState::Settled => "settled",
        }
    }
}

/// What the effect did, as settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EffectOutcome {
    Succeeded,
    Failed,
    /// It is not known whether the effect happened (a crash after the claim, a handler that
    /// reports uncertainty, a reconciler that cannot tell). Never read as success or failure.
    OutcomeUnknown,
}

/// The decision a principal makes about a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Decision {
    Approve,
    Deny,
    /// Withdraw an approval (or a pending request) before it is claimed.
    Revoke,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Approve => "approve",
            Decision::Deny => "deny",
            Decision::Revoke => "revoke",
        }
    }
}

/// One request and everything the workflow knows about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ApprovalRecord {
    pub id: String,
    pub action: String,
    /// Lowercase hex digest (64 digits) of the exact effect being approved.
    pub effect_digest: String,
    pub account: Option<String>,
    pub client: Option<String>,
    pub requester: String,
    /// Principals allowed to decide; empty means any verified principal that passes the account
    /// scope.
    pub approvers: BTreeSet<String>,
    /// Redacted ([`redact_detail`]).
    pub detail: String,
    pub created_ms: u64,
    pub expires_ms: u64,
    /// Starts at 1 and increases by one with every transition. A decision binds to it.
    pub revision: u64,
    pub state: ApprovalState,
    pub audit: AuditRequirement,
    pub reconcilable: bool,
    pub decided_by: Option<String>,
    pub decided_ms: Option<u64>,
    pub claim_id: Option<String>,
    pub claimed_ms: Option<u64>,
    pub outcome: Option<EffectOutcome>,
    pub settled_ms: Option<u64>,
    /// Why a request ended without an effect, or how a claim was resolved (redacted).
    pub reason: Option<String>,
}

/// A request to create.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct NewRequest {
    pub id: Option<String>,
    pub action: String,
    pub effect_digest: String,
    pub account: Option<String>,
    pub client: Option<String>,
    pub requester: String,
    pub ttl_ms: Option<u64>,
    pub approvers: BTreeSet<String>,
    pub detail: String,
}

impl NewRequest {
    pub fn new(
        action: impl Into<String>,
        effect_digest: impl Into<String>,
        requester: impl Into<String>,
    ) -> Self {
        Self {
            id: None,
            action: action.into(),
            effect_digest: effect_digest.into(),
            account: None,
            client: None,
            requester: requester.into(),
            ttl_ms: None,
            approvers: BTreeSet::new(),
            detail: String::new(),
        }
    }

    /// Use the app's own request id (Pulse's engine request id) instead of a generated one.
    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn account(mut self, account: impl Into<String>) -> Self {
        self.account = Some(account.into());
        self
    }

    pub fn client(mut self, client: impl Into<String>) -> Self {
        self.client = Some(client.into());
        self
    }

    pub fn ttl_ms(mut self, ttl_ms: u64) -> Self {
        self.ttl_ms = Some(ttl_ms);
        self
    }

    pub fn approvers<I, S>(mut self, approvers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.approvers = approvers.into_iter().map(Into::into).collect();
        self
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

/// Keep `detail` only when it is made of `[A-Za-z0-9_=. -]` (and short); otherwise the whole
/// detail is replaced by `detail=withheld`, never a partial copy.
pub fn redact_detail(detail: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "_=. -".contains(c);
    let trimmed = detail.trim();
    if trimmed.len() <= MAX_DETAIL && trimmed.chars().all(plain) {
        trimmed.to_string()
    } else {
        "detail=withheld".to_string()
    }
}

// ---------------------------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------------------------

/// Which accounts a verified principal may decide for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountScope {
    /// Not account-restricted (Pulse's owner key).
    Any,
    /// Only these accounts (Mail's per-account decisions).
    Only(BTreeSet<String>),
}

impl AccountScope {
    fn permits(&self, account: Option<&str>) -> bool {
        match self {
            AccountScope::Any => true,
            AccountScope::Only(set) => account.is_some_and(|a| set.contains(a)),
        }
    }
}

/// Who the verifier proved decided, and what that principal may decide for.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct VerifiedPrincipal {
    /// The principal's stable id (a device id, an enrolled key id, a local user).
    pub id: String,
    pub accounts: AccountScope,
    /// Claims the verifier established, kept for the app's audit formatting. Not interpreted.
    pub claims: BTreeMap<String, String>,
}

impl VerifiedPrincipal {
    pub fn new(id: impl Into<String>, accounts: AccountScope) -> Self {
        Self {
            id: id.into(),
            accounts,
            claims: BTreeMap::new(),
        }
    }

    pub fn claim(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.claims.insert(key.into(), value.into());
        self
    }
}

/// A nonce the proof carried, to be accepted once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nonce {
    pub value: String,
    /// When the proof stops being valid; the store may forget the nonce after it.
    pub expires_ms: u64,
}

/// A successful verification.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Verified {
    pub principal: VerifiedPrincipal,
    pub nonce: Option<Nonce>,
}

impl Verified {
    pub fn new(principal: VerifiedPrincipal) -> Self {
        Self {
            principal,
            nonce: None,
        }
    }

    pub fn with_nonce(mut self, value: impl Into<String>, expires_ms: u64) -> Self {
        self.nonce = Some(Nonce {
            value: value.into(),
            expires_ms,
        });
        self
    }
}

/// Why a proof was refused. The text is for the caller's log; it reaches the error returned to
/// the app but never the record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyError {
    reason: String,
}

impl VerifyError {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for VerifyError {}

/// What the verifier is asked to check: the **stored** request, the decision being made, and the
/// caller's proof. The verifier must bind the proof to `record` (id, action, effect digest,
/// account, client, expiry, revision) and to `decision`, so a proof made for another request or
/// another decision fails here.
#[derive(Debug)]
pub struct VerifyInput<'a> {
    pub record: &'a ApprovalRecord,
    pub decision: Decision,
    /// The caller's proof, opaque to this crate (a signature and its fields, a local session
    /// token, a database row id).
    pub proof: &'a [u8],
    pub now_ms: u64,
}

/// The app's native verification: a Secure Enclave or P-256 signature checked against the
/// enrolled key registry, a Mail user session, a local unlock. It must be fail-closed (any doubt
/// is `Err`) and must not retain the proof.
pub trait ApprovalVerifier: Send + Sync {
    fn verify(&self, input: &VerifyInput<'_>) -> Result<Verified, VerifyError>;
}

// ---------------------------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------------------------

/// Why a store call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StoreError {
    /// `insert` of an id that exists.
    Exists,
    /// `replace` of an id that does not exist.
    NotFound,
    /// `replace` found a revision other than the expected one.
    Conflict,
    /// The store is at a configured bound.
    Full,
    Unavailable(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Exists => f.write_str("the request id already exists"),
            StoreError::NotFound => f.write_str("the request does not exist"),
            StoreError::Conflict => f.write_str("the request changed since it was read"),
            StoreError::Full => f.write_str("the approval store is full"),
            StoreError::Unavailable(why) => write!(f, "the approval store is unavailable: {why}"),
        }
    }
}

impl std::error::Error for StoreError {}

/// The durable half of the workflow. Implementations must make `replace` an atomic
/// compare-and-set on `revision` and durable on `Ok`, and `claim_nonce` atomic; the coordinator's
/// one-claim and replay guarantees are exactly these two operations.
pub trait ApprovalStore: Send + Sync {
    /// Store a new request. `Err(Exists)` when the id is taken.
    fn insert(&self, record: &ApprovalRecord) -> Result<(), StoreError>;

    fn get(&self, id: &str) -> Result<Option<ApprovalRecord>, StoreError>;

    /// Replace the stored record with `record` only if the stored revision equals
    /// `expected_revision`; `Err(Conflict)` otherwise, `Err(NotFound)` for an unknown id.
    fn replace(&self, expected_revision: u64, record: &ApprovalRecord) -> Result<(), StoreError>;

    /// Every record in one of `states`, oldest first.
    fn list(&self, states: &[ApprovalState]) -> Result<Vec<ApprovalRecord>, StoreError>;

    /// Remember `value` until `expires_ms`. `Ok(true)` when it was new, `Ok(false)` when it was
    /// already remembered (a replay). `now_ms` lets the store forget expired nonces.
    fn claim_nonce(&self, value: &str, expires_ms: u64, now_ms: u64) -> Result<bool, StoreError>;
}

#[derive(Default)]
struct MemoryInner {
    records: BTreeMap<String, ApprovalRecord>,
    nonces: BTreeMap<String, u64>,
}

/// An in-process [`ApprovalStore`]. Cloning shares the state, so two coordinators over clones
/// model two threads (or, with a fresh coordinator over the same store, a restart that kept the
/// durable half). A real restart of the process forgets everything.
#[derive(Clone, Default)]
pub struct MemoryApprovalStore {
    inner: Arc<Mutex<MemoryInner>>,
}

impl MemoryApprovalStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, MemoryInner> {
        // A panic while the lock was held cannot leave a half-applied update: every mutation is a
        // single map operation.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl ApprovalStore for MemoryApprovalStore {
    fn insert(&self, record: &ApprovalRecord) -> Result<(), StoreError> {
        let mut inner = self.lock();
        if inner.records.contains_key(&record.id) {
            return Err(StoreError::Exists);
        }
        inner.records.insert(record.id.clone(), record.clone());
        Ok(())
    }

    fn get(&self, id: &str) -> Result<Option<ApprovalRecord>, StoreError> {
        Ok(self.lock().records.get(id).cloned())
    }

    fn replace(&self, expected_revision: u64, record: &ApprovalRecord) -> Result<(), StoreError> {
        let mut inner = self.lock();
        match inner.records.get_mut(&record.id) {
            None => Err(StoreError::NotFound),
            Some(stored) if stored.revision != expected_revision => Err(StoreError::Conflict),
            Some(stored) => {
                *stored = record.clone();
                Ok(())
            }
        }
    }

    fn list(&self, states: &[ApprovalState]) -> Result<Vec<ApprovalRecord>, StoreError> {
        let mut out: Vec<ApprovalRecord> = self
            .lock()
            .records
            .values()
            .filter(|r| states.contains(&r.state))
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            a.created_ms
                .cmp(&b.created_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(out)
    }

    fn claim_nonce(&self, value: &str, expires_ms: u64, now_ms: u64) -> Result<bool, StoreError> {
        let mut inner = self.lock();
        inner.nonces.retain(|_, until| *until > now_ms);
        if inner.nonces.contains_key(value) {
            return Ok(false);
        }
        if inner.nonces.len() >= MAX_NONCES {
            return Err(StoreError::Full);
        }
        inner.nonces.insert(value.to_string(), expires_ms);
        Ok(true)
    }
}

// ---------------------------------------------------------------------------------------------
// Audit
// ---------------------------------------------------------------------------------------------

/// What an audit row records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuditKind {
    Requested,
    Approved,
    Denied,
    Revoked,
    Cancelled,
    Expired,
    Claimed,
    Succeeded,
    Failed,
    OutcomeUnknown,
}

impl AuditKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditKind::Requested => "requested",
            AuditKind::Approved => "approved",
            AuditKind::Denied => "denied",
            AuditKind::Revoked => "revoked",
            AuditKind::Cancelled => "cancelled",
            AuditKind::Expired => "expired",
            AuditKind::Claimed => "claimed",
            AuditKind::Succeeded => "succeeded",
            AuditKind::Failed => "failed",
            AuditKind::OutcomeUnknown => "outcome_unknown",
        }
    }
}

/// One attempted transition, with its candidate revision. A later store conflict may prevent
/// commitment. Every free-text field has been through [`redact_detail`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AuditEvent {
    pub request_id: String,
    pub action: String,
    pub account: Option<String>,
    pub kind: AuditKind,
    /// The deciding principal or requester, when there is one.
    pub actor: Option<String>,
    pub detail: String,
    pub revision: u64,
    pub at_ms: u64,
}

impl AuditEvent {
    /// A one-line form with a fixed field order, for a log or a plain audit column.
    pub fn line(&self) -> String {
        format!(
            "approval={} event={} action={} account={} actor={} revision={} detail={}",
            self.request_id,
            self.kind.as_str(),
            self.action,
            self.account.as_deref().unwrap_or("-"),
            self.actor.as_deref().unwrap_or("-"),
            self.revision,
            self.detail
        )
    }
}

/// Why an append failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditError {
    reason: String,
}

impl AuditError {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl fmt::Display for AuditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for AuditError {}

/// The app's durable audit log. `Ok` means the row is durable.
pub trait AuditSink: Send + Sync {
    fn append(&self, event: &AuditEvent) -> Result<(), AuditError>;
}

// ---------------------------------------------------------------------------------------------
// Reconciliation
// ---------------------------------------------------------------------------------------------

/// What an idempotent effect says about an unsettled claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Reconciled {
    Applied,
    NotApplied,
    /// It cannot be told.
    Unknown,
}

/// For actions declared `reconcilable`: ask the effect whether a claimed request happened (a
/// sent-mail ledger, an idempotency key). Never consulted for other actions.
pub trait EffectReconciler: Send + Sync {
    fn reconcile(&self, record: &ApprovalRecord) -> Reconciled;
}

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

/// Why a workflow call was refused.
#[derive(Debug)]
#[non_exhaustive]
pub enum ApprovalError {
    /// The action is not in the profile.
    UnknownAction(String),
    /// A field failed validation.
    Invalid(&'static str),
    /// The request id exists.
    Duplicate(String),
    NotFound(String),
    TooManyOpen(usize),
    /// The action requires audit before effect and no sink is configured.
    AuditRequired,
    /// The call is not allowed from the request's current state.
    InvalidState {
        id: String,
        state: ApprovalState,
    },
    /// `claim` on a request that is still Pending.
    NotApproved(String),
    /// `claim` on a request that is already Claimed (or settled): the one claim is spent.
    AlreadyClaimed(String),
    /// A token that is not the one on record.
    StaleClaim(String),
    /// The request expired (and is now `Expired`).
    Expired(String),
    /// The decision named a revision other than the current one.
    StaleRevision {
        expected: u64,
        found: u64,
    },
    /// Another caller changed the request first.
    Conflict(String),
    /// The verifier refused the proof.
    Unverified(String),
    /// The principal's accounts do not cover the request's account.
    ScopeDenied,
    /// The request names approvers and the principal is not one.
    NotAnApprover,
    /// The proof's nonce was already used.
    ReplayedNonce,
    /// Only the requester may cancel.
    NotRequester,
    /// A required audit row could not be appended; see the operation for what that left.
    AuditFailed(String),
    Store(StoreError),
}

impl fmt::Display for ApprovalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApprovalError::UnknownAction(a) => write!(f, "approval: action {a:?} is not declared"),
            ApprovalError::Invalid(why) => write!(f, "approval: {why}"),
            ApprovalError::Duplicate(id) => write!(f, "approval: request {id} already exists"),
            ApprovalError::NotFound(id) => write!(f, "approval: request {id} does not exist"),
            ApprovalError::TooManyOpen(n) => write!(f, "approval: {n} requests are already open"),
            ApprovalError::AuditRequired => {
                f.write_str("approval: the action requires audit before effect and no sink is set")
            }
            ApprovalError::InvalidState { id, state } => {
                write!(f, "approval: request {id} is {}", state.as_str())
            }
            ApprovalError::NotApproved(id) => write!(f, "approval: request {id} is not approved"),
            ApprovalError::AlreadyClaimed(id) => {
                write!(f, "approval: request {id} was already claimed")
            }
            ApprovalError::StaleClaim(id) => {
                write!(
                    f,
                    "approval: the claim on request {id} is not the one on record"
                )
            }
            ApprovalError::Expired(id) => write!(f, "approval: request {id} expired"),
            ApprovalError::StaleRevision { expected, found } => write!(
                f,
                "approval: the decision is for revision {expected}, the request is at {found}"
            ),
            ApprovalError::Conflict(id) => {
                write!(f, "approval: request {id} changed concurrently")
            }
            ApprovalError::Unverified(why) => write!(f, "approval: proof refused: {why}"),
            ApprovalError::ScopeDenied => {
                f.write_str("approval: the principal may not decide for this account")
            }
            ApprovalError::NotAnApprover => {
                f.write_str("approval: the principal is not an approver of this request")
            }
            ApprovalError::ReplayedNonce => f.write_str("approval: the proof was already used"),
            ApprovalError::NotRequester => {
                f.write_str("approval: only the requester may cancel a request")
            }
            ApprovalError::AuditFailed(why) => write!(f, "approval: audit row not written: {why}"),
            ApprovalError::Store(e) => write!(f, "approval: {e}"),
        }
    }
}

impl std::error::Error for ApprovalError {}

impl From<StoreError> for ApprovalError {
    fn from(e: StoreError) -> Self {
        ApprovalError::Store(e)
    }
}

// ---------------------------------------------------------------------------------------------
// Coordinator
// ---------------------------------------------------------------------------------------------

/// The one right to execute an approved request. Not `Clone`: it is handed out once per request
/// and presented to [`ApprovalCoordinator::settle`].
#[derive(Debug)]
#[non_exhaustive]
pub struct ClaimToken {
    pub request_id: String,
    pub claim_id: String,
    pub action: String,
    pub effect_digest: String,
    pub account: Option<String>,
    pub client: Option<String>,
    /// The revision of the `Claimed` record.
    pub revision: u64,
}

/// Which unsettled claims [`ApprovalCoordinator::recover_claims`] resolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimRecovery {
    /// Every claim: for startup, before any handler runs. At runtime it would resolve claims
    /// whose handler is still working, which then fail to settle ([`ApprovalError::StaleClaim`]).
    All,
    /// Only claims older than this many milliseconds (a handler timeout).
    OlderThan(u64),
}

/// The workflow coordinator. Cheap to share (`&self` everywhere); all cross-caller exclusion is
/// the store's compare-and-set.
pub struct ApprovalCoordinator {
    store: Arc<dyn ApprovalStore>,
    verifier: Arc<dyn ApprovalVerifier>,
    audit: Option<Arc<dyn AuditSink>>,
    clock: Arc<dyn ApprovalClock>,
    profile: ApprovalProfile,
    audit_failures: AtomicU64,
}

impl ApprovalCoordinator {
    /// A coordinator over `store`, verifying with `verifier`, under `profile`. Fails when the
    /// profile is invalid.
    pub fn new(
        profile: ApprovalProfile,
        store: Arc<dyn ApprovalStore>,
        verifier: Arc<dyn ApprovalVerifier>,
    ) -> Result<Self, ApprovalError> {
        profile.validate()?;
        Ok(Self {
            store,
            verifier,
            audit: None,
            clock: Arc::new(SystemApprovalClock),
            profile,
            audit_failures: AtomicU64::new(0),
        })
    }

    pub fn with_audit(mut self, sink: Arc<dyn AuditSink>) -> Self {
        self.audit = Some(sink);
        self
    }

    pub fn with_clock(mut self, clock: Arc<dyn ApprovalClock>) -> Self {
        self.clock = clock;
        self
    }

    /// How many best-effort audit appends failed since this coordinator was built.
    pub fn audit_failures(&self) -> u64 {
        self.audit_failures.load(Ordering::SeqCst)
    }

    pub fn get(&self, id: &str) -> Result<Option<ApprovalRecord>, ApprovalError> {
        Ok(self.store.get(id)?)
    }

    fn load(&self, id: &str) -> Result<ApprovalRecord, ApprovalError> {
        self.store
            .get(id)?
            .ok_or_else(|| ApprovalError::NotFound(id.to_string()))
    }

    fn event(
        &self,
        record: &ApprovalRecord,
        kind: AuditKind,
        actor: Option<&str>,
        detail: &str,
    ) -> AuditEvent {
        AuditEvent {
            request_id: record.id.clone(),
            action: record.action.clone(),
            account: record.account.clone(),
            kind,
            actor: actor.map(str::to_string),
            detail: redact_detail(detail),
            revision: record.revision,
            at_ms: self.clock.now_ms(),
        }
    }

    fn audit_best_effort(&self, event: AuditEvent) {
        if let Some(sink) = &self.audit {
            if sink.append(&event).is_err() {
                self.audit_failures.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    /// Append `event` according to the record's requirement: `BeforeEffect` makes a failure an
    /// error, anything else counts it.
    fn audit_for(&self, record: &ApprovalRecord, event: AuditEvent) -> Result<(), ApprovalError> {
        match (&self.audit, record.audit) {
            (Some(sink), AuditRequirement::BeforeEffect) => sink
                .append(&event)
                .map_err(|e| ApprovalError::AuditFailed(e.to_string())),
            (None, AuditRequirement::BeforeEffect) => Err(ApprovalError::AuditRequired),
            _ => {
                self.audit_best_effort(event);
                Ok(())
            }
        }
    }

    /// Replace `current` with the edited copy, revision plus one, through the store's
    /// compare-and-set.
    fn transition(
        &self,
        current: &ApprovalRecord,
        kind: AuditKind,
        actor: Option<&str>,
        detail: &str,
        edit: impl FnOnce(&mut ApprovalRecord),
    ) -> Result<ApprovalRecord, ApprovalError> {
        let mut next = current.clone();
        edit(&mut next);
        next.revision = current
            .revision
            .checked_add(1)
            .ok_or(ApprovalError::Invalid("approval revision exhausted"))?;
        self.audit_for(&next, self.event(&next, kind, actor, detail))?;
        match self.store.replace(current.revision, &next) {
            Ok(()) => Ok(next),
            Err(StoreError::Conflict) => Err(ApprovalError::Conflict(current.id.clone())),
            Err(e) => Err(ApprovalError::Store(e)),
        }
    }

    /// If `record` is open and due, move it to `Expired` and return the expired record.
    /// `Ok(None)` when it is not due. A concurrent change that wins the race is not an error: the
    /// caller re-reads.
    fn expire_if_due(
        &self,
        record: &ApprovalRecord,
    ) -> Result<Option<ApprovalRecord>, ApprovalError> {
        if !record.state.is_open() || self.clock.now_ms() < record.expires_ms {
            return Ok(None);
        }
        match self.transition(record, AuditKind::Expired, None, "expired", |r| {
            r.state = ApprovalState::Expired;
            r.reason = Some("expired".to_string());
        }) {
            Ok(next) => Ok(Some(next)),
            Err(ApprovalError::Conflict(_)) => {
                // Someone else moved it (a decision, a cancel); report what is there now.
                let now = self.load(&record.id)?;
                Ok(if now.state == ApprovalState::Expired {
                    Some(now)
                } else {
                    None
                })
            }
            Err(e) => Err(e),
        }
    }

    // ---- request ---------------------------------------------------------------------------

    /// Open a request. The request is bound to `effect_digest` for its whole life.
    pub fn request(&self, new: NewRequest) -> Result<ApprovalRecord, ApprovalError> {
        let profile = self
            .profile
            .get(&new.action)
            .ok_or_else(|| ApprovalError::UnknownAction(new.action.clone()))?;
        if profile.audit == AuditRequirement::BeforeEffect && self.audit.is_none() {
            return Err(ApprovalError::AuditRequired);
        }
        let digest_ok = new.effect_digest.len() == 64
            && new
                .effect_digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !digest_ok {
            return Err(ApprovalError::Invalid(
                "effect digest must be 64 lowercase hex digits",
            ));
        }
        if !valid_label(&new.requester) {
            return Err(ApprovalError::Invalid("requester is not a plain label"));
        }
        for (present, label) in [(&new.account, "account"), (&new.client, "client")] {
            if let Some(value) = present {
                if !valid_label(value) {
                    return Err(ApprovalError::Invalid(match label {
                        "account" => "account is not a plain label",
                        _ => "client is not a plain label",
                    }));
                }
            }
        }
        if profile.requires_account && new.account.is_none() {
            return Err(ApprovalError::Invalid("the action requires an account"));
        }
        if profile.requires_client && new.client.is_none() {
            return Err(ApprovalError::Invalid("the action requires a client"));
        }
        if new.approvers.iter().any(|a| !valid_label(a)) {
            return Err(ApprovalError::Invalid("an approver is not a plain label"));
        }
        let ttl = new.ttl_ms.unwrap_or(profile.default_ttl_ms);
        if ttl == 0 || ttl > profile.max_ttl_ms {
            return Err(ApprovalError::Invalid(
                "lifetime must be positive and within the action's maximum",
            ));
        }
        let id = match &new.id {
            Some(id) if valid_label(id) && id.len() <= 128 => id.clone(),
            Some(_) => return Err(ApprovalError::Invalid("request id is not a plain label")),
            None => random_id()?,
        };
        self.sweep_expired()?;
        let open = self
            .store
            .list(&[ApprovalState::Pending, ApprovalState::Approved])?
            .len();
        if open >= self.profile.max_open {
            return Err(ApprovalError::TooManyOpen(open));
        }
        let now = self.clock.now_ms();
        let record = ApprovalRecord {
            id,
            action: new.action,
            effect_digest: new.effect_digest,
            account: new.account,
            client: new.client,
            requester: new.requester,
            approvers: new.approvers,
            detail: redact_detail(&new.detail),
            created_ms: now,
            expires_ms: now.saturating_add(ttl),
            revision: 1,
            state: ApprovalState::Pending,
            audit: profile.audit,
            reconcilable: profile.reconcilable,
            decided_by: None,
            decided_ms: None,
            claim_id: None,
            claimed_ms: None,
            outcome: None,
            settled_ms: None,
            reason: None,
        };
        let event = self.event(&record, AuditKind::Requested, Some(&record.requester), "");
        self.audit_for(&record, event)?;
        match self.store.insert(&record) {
            Ok(()) => {}
            Err(StoreError::Exists) => return Err(ApprovalError::Duplicate(record.id)),
            Err(e) => return Err(ApprovalError::Store(e)),
        }
        Ok(record)
    }

    // ---- decide ----------------------------------------------------------------------------

    /// Apply a decision. `revision` is the revision the approver saw; `proof` goes to the
    /// [`ApprovalVerifier`] untouched.
    ///
    /// `Approve` and `Deny` need the request `Pending`; `Revoke` needs it `Pending` or
    /// `Approved` (a claimed request cannot be revoked: its effect may be running).
    pub fn decide(
        &self,
        id: &str,
        decision: Decision,
        revision: u64,
        proof: &[u8],
    ) -> Result<ApprovalRecord, ApprovalError> {
        let record = self.load(id)?;
        if record.revision != revision {
            return Err(ApprovalError::StaleRevision {
                expected: revision,
                found: record.revision,
            });
        }
        if let Some(expired) = self.expire_if_due(&record)? {
            return Err(ApprovalError::Expired(expired.id));
        }
        let allowed = match decision {
            Decision::Approve | Decision::Deny => record.state == ApprovalState::Pending,
            Decision::Revoke => record.state.is_open(),
        };
        if !allowed {
            return Err(ApprovalError::InvalidState {
                id: record.id,
                state: record.state,
            });
        }
        let now = self.clock.now_ms();
        let verified = self
            .verifier
            .verify(&VerifyInput {
                record: &record,
                decision,
                proof,
                now_ms: now,
            })
            .map_err(|e| ApprovalError::Unverified(e.to_string()))?;
        if !verified
            .principal
            .accounts
            .permits(record.account.as_deref())
        {
            return Err(ApprovalError::ScopeDenied);
        }
        if !record.approvers.is_empty() && !record.approvers.contains(&verified.principal.id) {
            return Err(ApprovalError::NotAnApprover);
        }
        if let Some(nonce) = &verified.nonce {
            if nonce.expires_ms <= now {
                return Err(ApprovalError::Unverified(
                    "the proof has expired".to_string(),
                ));
            }
            if !self
                .store
                .claim_nonce(&nonce.value, nonce.expires_ms, now)?
            {
                return Err(ApprovalError::ReplayedNonce);
            }
        }
        let actor = verified.principal.id.clone();
        let (state, kind) = match decision {
            Decision::Approve => (ApprovalState::Approved, AuditKind::Approved),
            Decision::Deny => (ApprovalState::Denied, AuditKind::Denied),
            Decision::Revoke => (ApprovalState::Revoked, AuditKind::Revoked),
        };
        let next = self.transition(&record, kind, Some(&actor), decision.as_str(), |r| {
            r.state = state;
            r.decided_by = Some(actor.clone());
            r.decided_ms = Some(now);
        })?;
        Ok(next)
    }

    /// The requester withdraws an open request.
    pub fn cancel(&self, id: &str, requester: &str) -> Result<ApprovalRecord, ApprovalError> {
        let record = self.load(id)?;
        if record.requester != requester {
            return Err(ApprovalError::NotRequester);
        }
        if let Some(expired) = self.expire_if_due(&record)? {
            return Err(ApprovalError::Expired(expired.id));
        }
        if !record.state.is_open() {
            return Err(ApprovalError::InvalidState {
                id: record.id,
                state: record.state,
            });
        }
        let next = self.transition(
            &record,
            AuditKind::Cancelled,
            Some(requester),
            "cancelled",
            |r| {
                r.state = ApprovalState::Cancelled;
                r.reason = Some("cancelled".to_string());
            },
        )?;
        Ok(next)
    }

    /// Revoke every open request of `account` (the account was removed or disabled). Claimed
    /// requests are left alone. Returns how many were revoked.
    pub fn revoke_account(&self, account: &str, reason: &str) -> Result<usize, ApprovalError> {
        let reason = redact_detail(reason);
        let mut revoked = 0;
        for record in self
            .store
            .list(&[ApprovalState::Pending, ApprovalState::Approved])?
        {
            if record.account.as_deref() != Some(account) {
                continue;
            }
            match self.transition(&record, AuditKind::Revoked, None, &reason, |r| {
                r.state = ApprovalState::Revoked;
                r.reason = Some(reason.clone());
            }) {
                Ok(_) => revoked += 1,
                // A concurrent decision or claim won: it is no longer the open request this call
                // meant to revoke (a claim in flight is deliberately not revoked).
                Err(ApprovalError::Conflict(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(revoked)
    }

    /// Move every open request that is past its expiry to `Expired`. Returns how many.
    pub fn sweep_expired(&self) -> Result<usize, ApprovalError> {
        let mut expired = 0;
        for record in self
            .store
            .list(&[ApprovalState::Pending, ApprovalState::Approved])?
        {
            if self.expire_if_due(&record)?.is_some() {
                expired += 1;
            }
        }
        Ok(expired)
    }

    // ---- claim and settle ------------------------------------------------------------------

    /// Take the one right to execute an approved request. Exactly one of any number of
    /// concurrent callers gets the token; the others see [`ApprovalError::AlreadyClaimed`] (or
    /// `Conflict` when they lost the race on the same revision).
    ///
    /// With [`AuditRequirement::BeforeEffect`] the `Claimed` row is appended before the
    /// compare-and-set. A failed append leaves `Approved` unchanged and returns no token.
    pub fn claim(&self, id: &str) -> Result<ClaimToken, ApprovalError> {
        let record = self.load(id)?;
        match record.state {
            ApprovalState::Approved => {}
            ApprovalState::Pending => return Err(ApprovalError::NotApproved(record.id)),
            ApprovalState::Claimed | ApprovalState::Settled => {
                return Err(ApprovalError::AlreadyClaimed(record.id))
            }
            state => {
                return Err(ApprovalError::InvalidState {
                    id: record.id,
                    state,
                })
            }
        }
        if let Some(expired) = self.expire_if_due(&record)? {
            return Err(ApprovalError::Expired(expired.id));
        }
        if record.audit == AuditRequirement::BeforeEffect && self.audit.is_none() {
            return Err(ApprovalError::AuditRequired);
        }
        let now = self.clock.now_ms();
        let claim_id = random_id()?;
        let claimed = match self.transition(
            &record,
            AuditKind::Claimed,
            record.decided_by.as_deref(),
            "",
            |r| {
                r.state = ApprovalState::Claimed;
                r.claim_id = Some(claim_id.clone());
                r.claimed_ms = Some(now);
            },
        ) {
            Ok(next) => next,
            Err(ApprovalError::Conflict(_)) => {
                // Lost the race: say what the winner made of it.
                return Err(match self.store.get(id)? {
                    Some(r)
                        if matches!(r.state, ApprovalState::Claimed | ApprovalState::Settled) =>
                    {
                        ApprovalError::AlreadyClaimed(r.id)
                    }
                    Some(r) if r.state.is_terminal() => ApprovalError::InvalidState {
                        id: r.id,
                        state: r.state,
                    },
                    _ => ApprovalError::Conflict(id.to_string()),
                });
            }
            Err(e) => return Err(e),
        };
        Ok(ClaimToken {
            request_id: claimed.id,
            claim_id,
            action: claimed.action,
            effect_digest: claimed.effect_digest,
            account: claimed.account,
            client: claimed.client,
            revision: claimed.revision,
        })
    }

    /// Record what the effect did. Accepts only the token whose claim id is on record. With
    /// [`AuditRequirement::BeforeEffect`] the outcome row is appended first; if that fails the
    /// call returns [`ApprovalError::AuditFailed`], the request stays `Claimed` and the caller
    /// must not report success (it may retry with the same token).
    pub fn settle(
        &self,
        token: &ClaimToken,
        outcome: EffectOutcome,
        detail: &str,
    ) -> Result<ApprovalRecord, ApprovalError> {
        let record = self.load(&token.request_id)?;
        if record.state != ApprovalState::Claimed
            || record.claim_id.as_deref() != Some(token.claim_id.as_str())
        {
            return Err(ApprovalError::StaleClaim(token.request_id.clone()));
        }
        let detail = redact_detail(detail);
        let kind = match outcome {
            EffectOutcome::Succeeded => AuditKind::Succeeded,
            EffectOutcome::Failed => AuditKind::Failed,
            EffectOutcome::OutcomeUnknown => AuditKind::OutcomeUnknown,
        };
        let now = self.clock.now_ms();
        self.transition(&record, kind, record.decided_by.as_deref(), &detail, |r| {
            r.state = ApprovalState::Settled;
            r.outcome = Some(outcome);
            r.settled_ms = Some(now);
            r.reason = Some(detail.clone());
        })
        .map_err(|e| match e {
            // Recovery resolved the claim while the effect ran: the token is stale now.
            ApprovalError::Conflict(id) => ApprovalError::StaleClaim(id),
            other => other,
        })
    }

    /// Resolve claims that were never settled (a crash, a handler that disappeared).
    ///
    /// A request whose action is `reconcilable` and a `reconciler` is given: `Applied` settles
    /// it `Succeeded`, `NotApplied` settles it `Failed` (the effect did not happen; a new request
    /// is needed to try again), `Unknown` settles it `OutcomeUnknown`. Every other claim settles
    /// as `OutcomeUnknown`: the coordinator cannot know whether an external effect happened and
    /// never assumes it did not. Required audit rows gate recovery success as they gate settle.
    /// Returns the settled records.
    pub fn recover_claims(
        &self,
        which: ClaimRecovery,
        reconciler: Option<&dyn EffectReconciler>,
    ) -> Result<Vec<ApprovalRecord>, ApprovalError> {
        let now = self.clock.now_ms();
        let mut settled = Vec::new();
        for record in self.store.list(&[ApprovalState::Claimed])? {
            if let ClaimRecovery::OlderThan(age) = which {
                let claimed = record.claimed_ms.unwrap_or(0);
                if now.saturating_sub(claimed) < age {
                    continue;
                }
            }
            let (outcome, why) = match (record.reconcilable, reconciler) {
                (true, Some(r)) => match r.reconcile(&record) {
                    Reconciled::Applied => (EffectOutcome::Succeeded, "reconciled_applied"),
                    Reconciled::NotApplied => (EffectOutcome::Failed, "reconciled_not_applied"),
                    Reconciled::Unknown => (EffectOutcome::OutcomeUnknown, "reconciled_unknown"),
                },
                _ => (EffectOutcome::OutcomeUnknown, "unsettled_claim"),
            };
            let kind = match outcome {
                EffectOutcome::Succeeded => AuditKind::Succeeded,
                EffectOutcome::Failed => AuditKind::Failed,
                EffectOutcome::OutcomeUnknown => AuditKind::OutcomeUnknown,
            };
            match self.transition(&record, kind, None, why, |r| {
                r.state = ApprovalState::Settled;
                r.outcome = Some(outcome);
                r.settled_ms = Some(now);
                r.reason = Some(why.to_string());
            }) {
                Ok(next) => {
                    settled.push(next);
                }
                // The handler settled it between the listing and now.
                Err(ApprovalError::Conflict(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(settled)
    }
}

fn random_id() -> Result<String, ApprovalError> {
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw)
        .map_err(|_| ApprovalError::Invalid("the system random source is unavailable"))?;
    Ok(raw.iter().map(|b| format!("{b:02x}")).collect())
}
