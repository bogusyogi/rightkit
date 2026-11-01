//! Mid-turn credential request & resume (ENG-94).
//!
//! When a tool call discovers, mid-execution, that it needs a credential the
//! session does not currently hold (e.g. a Cloudflare API token, a Fly.io
//! deploy key, or some other provider secret not supplied at session start),
//! the turn should pause — the same way `engine_tool_dispatch_approval`
//! pauses a turn for a tool-approval prompt — ask the operator for the
//! specific credential, and resume the *same* pending tool call with the
//! supplied value rather than failing the whole turn.
//!
//! This module intentionally mirrors the `pending_approvals` oneshot shape in
//! `SessionEngine` (see `engine.rs` field `pending_approvals: Arc<Mutex<HashMap<String,
//! oneshot::Sender<bool>>>>` and its resolution in `engine_tool_dispatch_approval.rs`)
//! but resolves to a [`CredentialResume`] payload instead of a bare `bool`,
//! because the "approved" case must also carry the secret value itself.
//!
//! # Where the secret may and may not travel
//!
//! - The secret value is carried *only* inside [`CredentialResume::Supplied`],
//!   inside a [`SecretValue`] wrapper whose `Debug`/`Display` never print the
//!   inner bytes.
//! - The secret must never be placed on [`CredentialRequest`] (the
//!   outbound-to-UI/transcript side) — that struct carries only the
//!   credential's *identity* (name/kind) and the human-readable reason the
//!   tool needs it, both of which are safe to show in a transcript and log.
//! - The secret must never be passed to `rightkit_agent_policy::decision_log`.
//!   [`record_requested`] and [`record_outcome`] log only the request id,
//!   the credential name/kind, and the outcome tag (`supplied` / `declined` /
//!   `timed_out`) — never the value. Note that `DecisionLog::append` already
//!   scrubs any context key containing the substring `"credential"` (see
//!   `rightkit_agent_policy::decision_log::FORBIDDEN_CONTEXT_KEY_FRAGMENTS`), so
//!   this module deliberately uses the non-matching context key
//!   `"requested_for"` rather than `"credential_name"` — using a forbidden
//!   fragment here would silently drop the (harmless, non-secret) name too,
//!   which would degrade the audit record without protecting anything.
//! - Once [`PendingCredentialRequests::take_and_resolve`] delivers a [`CredentialResume::Supplied`] value
//!   to the waiting tool call, the sender side of the channel is removed from
//!   the registry (see "Single-use binding" below) so the same secret cannot
//!   be redelivered or replayed into a second call.
//!
//! # Single-use binding to one pending tool call
//!
//! Each [`CredentialRequest`] is created with the `tool_call_id` of the
//! specific pending call that needs the credential (mirroring how
//! `pending_approvals` keys its oneshot channel by `call.id`, not by tool
//! name or session). The registry below keys its oneshot sender by that same
//! `tool_call_id`:
//!
//! - [`PendingCredentialRequests::insert`] stores at most one sender per
//!   `tool_call_id`; inserting a second request for the same call id replaces
//!   (and thereby silently invalidates — the old receiver observes a dropped
//!   sender) any prior one, so a stale request can never be answered after a
//!   newer one superseded it.
//! - [`PendingCredentialRequests::take_and_resolve`] *removes* the entry
//!   before sending, so the exact same request can be resolved at most once:
//!   a second resolve attempt (e.g. a duplicate/late response from the UI)
//!   finds nothing to deliver to and returns `false`. This is the same
//!   removal-on-resolve discipline the existing approval flow uses
//!   (`self.pending_approvals.lock().unwrap().remove(&call.id)`).
//! - A oneshot channel is itself single-consumer/single-producer: even if a
//!   caller somehow retained the `Receiver`, only the first `send` is
//!   observable.
//!
//! # Wiring status
//!
//! `SessionEngine` (in `engine.rs`) now holds a `pending_credential_requests:
//! Arc<Mutex<PendingCredentialRequests>>` field, constructed the same way and
//! in the same place as `pending_approvals`. `engine_session_controls.rs`
//! exposes `set_credential_request_store` (mirroring `set_approval_store`),
//! `list_pending_credential_requests` (non-consuming), and
//! `resolve_credential_request` (single-use) so a daemon/FFI lane can reach
//! this registry without touching `SessionEngine`'s private fields directly.
//!
//! # Gaps (out of allowlist for this lane)
//!
//! - Nothing yet calls `PendingCredentialRequests::insert` to actually raise a
//!   request, and no `ProtocolEvent::CredentialRequested` /
//!   `CredentialResolved` pair exists yet (parallel to
//!   `ToolApprovalRequested` / `ToolApprovalResolved` in
//!   `rightkit_agent_engine_protocol`) to announce one over the wire. Raising a request
//!   from a running tool, and emitting those events, requires touching
//!   `engine_permission_flow.rs`, `engine_tool_dispatch_approval.rs`, the
//!   protocol crate, and `mobile_runtime/src/ffi.rs` — none of which are in
//!   this lane's allowlist.
//! - The actual "does the running tool have the credential it needs" check —
//!   i.e. where a tool would detect the gap and call into this module instead
//!   of just failing — is not implemented here; that requires touching each
//!   tool's `execute` path (e.g. `rightkit_agent_tools::coding_tools::cloudflare`,
//!   `rightkit_agent_tools::coding_tools::flyio`) and the dispatch loop in
//!   `engine_tool_dispatch_execute.rs`, none of which are in this lane's
//!   allowlist.
//! - Delivering a `Supplied` credential to the *running* tool call is
//!   designed to reuse the existing mid-session registries in
//!   `rightkit_agent_tools::coding_tools::context` (`update_cloudflare_credential`,
//!   `update_flyio_credential`, and their `*_credential` /
//!   `retire_*_credential` counterparts) — a resumed call should route the
//!   secret through those session-keyed cells rather than a new channel, so
//!   the next tool invocation for that session picks it up the same way a
//!   mid-session refresh does today. Wiring that hand-off (matching
//!   `CredentialKind` to the right `update_*_credential` call) is also
//!   outside this lane's allowlist.

use std::collections::HashMap;

/// Identity of the credential being requested. Deliberately an open string
/// enum member (`Other`) alongside the two credential kinds `rightkit_agent_tools::coding_tools`
/// already has mid-session refresh registries for, so a new provider doesn't
/// require touching this type — only the (out-of-allowlist) dispatch wiring
/// that maps a kind to its `update_*_credential` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialKind {
    Cloudflare,
    FlyIo,
    Other(String),
}

impl CredentialKind {
    /// Stable, non-secret label safe to show in a transcript or decision log.
    pub fn label(&self) -> &str {
        match self {
            CredentialKind::Cloudflare => "cloudflare",
            CredentialKind::FlyIo => "flyio",
            CredentialKind::Other(name) => name.as_str(),
        }
    }
}

/// A secret value that refuses to print itself. Holds the credential bytes
/// but neither `Debug` nor `Display` reveal them, so an accidental `{:?}` in
/// a log line, error message, or panic payload cannot leak the value.
#[derive(Clone)]
pub struct SecretValue(String);

impl SecretValue {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Explicit, deliberate access to the raw secret. Callers must not pass
    /// the result of this to anything that logs, serializes to a transcript,
    /// or otherwise persists it outside the credential registries in
    /// `rightkit_agent_tools::coding_tools::context`.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretValue(<redacted>)")
    }
}

impl std::fmt::Display for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

impl PartialEq for SecretValue {
    /// Constant-time-ish equality is not the goal here (this is an in-process
    /// resume payload, not a network comparison); this exists only so tests
    /// can assert on the value without a custom accessor, while `Debug` still
    /// never prints it.
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

/// A typed, single-use credential request bound to one pending tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialRequest {
    /// Id of the specific pending tool call that needs this credential.
    /// Mirrors `ToolCall::id` / the `pending_approvals` map key.
    pub tool_call_id: String,
    /// Which credential is needed.
    pub kind: CredentialKind,
    /// Human-readable reason the running tool needs this credential, shown
    /// to the operator. Must never contain the secret itself (there is none
    /// to contain at request time — the operator has not supplied it yet).
    pub reason: String,
}

impl CredentialRequest {
    pub fn new(
        tool_call_id: impl Into<String>,
        kind: CredentialKind,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            kind,
            reason: reason.into(),
        }
    }
}

/// The operator's response to a [`CredentialRequest`].
#[derive(Clone)]
pub enum CredentialResume {
    /// The operator supplied a value. Carried in a [`SecretValue`] so it
    /// cannot be accidentally printed on its way to the tool.
    Supplied(SecretValue),
    /// The operator declined to supply the credential; the paused tool call
    /// should fail with a normal (non-timeout) denial, the same way a
    /// declined tool approval does.
    Declined,
}

impl std::fmt::Debug for CredentialResume {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CredentialResume::Supplied(_) => f.write_str("CredentialResume::Supplied(<redacted>)"),
            CredentialResume::Declined => f.write_str("CredentialResume::Declined"),
        }
    }
}

/// Outcome tag safe to place in the decision log (never the secret itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialOutcome {
    Supplied,
    Declined,
    TimedOut,
}

impl CredentialOutcome {
    fn as_str(self) -> &'static str {
        match self {
            CredentialOutcome::Supplied => "supplied",
            CredentialOutcome::Declined => "declined",
            CredentialOutcome::TimedOut => "timed_out",
        }
    }
}

/// Session-agnostic registry of in-flight credential requests, keyed by
/// `tool_call_id`. Intended to be held by `SessionEngine` behind an
/// `Arc<Mutex<..>>`, exactly like the existing `pending_approvals` field
/// (see the module-level doc comment for why wiring that field is out of
/// this lane's allowlist).
#[derive(Default)]
pub struct PendingCredentialRequests {
    senders: HashMap<String, tokio::sync::oneshot::Sender<CredentialResume>>,
    /// Non-secret request metadata (kind/reason), kept alongside the sender
    /// so a session-scoped registry holder (e.g. `SessionEngine`) can
    /// enumerate what is currently pending — for a UI/FFI list view — without
    /// exposing or consuming the oneshot sender itself. Never holds a
    /// [`SecretValue`]: at insert time no value has been supplied yet, and
    /// resolving removes the entry rather than mutating it in place.
    requests: HashMap<String, CredentialRequest>,
}

impl PendingCredentialRequests {
    pub fn new() -> Self {
        Self {
            senders: HashMap::new(),
            requests: HashMap::new(),
        }
    }

    /// Register a new pending `request`, returning the receiver the paused
    /// turn should await. Inserting a second request for the same
    /// `tool_call_id` drops the previous sender (its receiver, if still
    /// awaited anywhere, observes a closed channel) — a superseded request
    /// can never be answered after the fact, preserving single-use binding
    /// to exactly one live request per call.
    pub fn insert(
        &mut self,
        request: CredentialRequest,
    ) -> tokio::sync::oneshot::Receiver<CredentialResume> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.senders.insert(request.tool_call_id.clone(), tx);
        self.requests.insert(request.tool_call_id.clone(), request);
        rx
    }

    /// Resolve the pending request for `tool_call_id` exactly once. Removes
    /// the entry before sending so a duplicate or late resolve call (e.g. a
    /// retried UI submission) finds nothing and returns `false` instead of
    /// silently no-op'ing on an already-consumed channel.
    pub fn take_and_resolve(&mut self, tool_call_id: &str, resume: CredentialResume) -> bool {
        self.requests.remove(tool_call_id);
        match self.senders.remove(tool_call_id) {
            Some(tx) => tx.send(resume).is_ok(),
            None => false,
        }
    }

    /// Drop the pending request for `tool_call_id` without resolving it
    /// (e.g. on timeout). The receiver observes a closed channel, which
    /// callers should treat as [`CredentialOutcome::TimedOut`] /
    /// equivalent-to-declined, matching the fail-closed timeout behavior of
    /// `pending_approvals`.
    pub fn remove(&mut self, tool_call_id: &str) {
        self.senders.remove(tool_call_id);
        self.requests.remove(tool_call_id);
    }

    pub fn is_empty(&self) -> bool {
        self.senders.is_empty()
    }

    /// Non-consuming snapshot of every currently pending request (identity +
    /// reason only — never a secret, since none is held here). Listing does
    /// not remove or otherwise invalidate anything: the sender for each
    /// listed id remains registered and single-use exactly as before this
    /// call, so a list operation can safely run concurrently with, or
    /// repeatedly before, a real resolve.
    pub fn pending(&self) -> Vec<CredentialRequest> {
        self.requests.values().cloned().collect()
    }
}

/// Record that a credential request was raised, into the ENG-93 decision
/// log. Logs only the request id, the credential kind's non-secret label,
/// and the reason string — never a value, because there is no value yet.
///
/// Callers must ensure `reason` itself was not constructed from tool output
/// that could embed a secret; this function has no way to detect that and
/// performs no scrubbing beyond the log's own forbidden-key-fragment filter.
pub fn record_requested(
    log: &mut rightkit_agent_policy::decision_log::DecisionLog,
    timestamp_ms: u64,
    request: &CredentialRequest,
) {
    log.append(
        timestamp_ms,
        rightkit_agent_policy::PermissionDecision::Ask,
        "credential_request:asked",
        "credential_request",
        vec![
            ("tool_call_id".to_string(), request.tool_call_id.clone()),
            (
                "requested_for".to_string(),
                request.kind.label().to_string(),
            ),
        ],
    );
}

/// Record the outcome of a previously-raised credential request. Logs the
/// outcome tag (`supplied` / `declined` / `timed_out`) only — never the
/// supplied value.
pub fn record_outcome(
    log: &mut rightkit_agent_policy::decision_log::DecisionLog,
    timestamp_ms: u64,
    request: &CredentialRequest,
    outcome: CredentialOutcome,
) {
    let decision = match outcome {
        CredentialOutcome::Supplied => rightkit_agent_policy::PermissionDecision::Allow,
        CredentialOutcome::Declined | CredentialOutcome::TimedOut => {
            rightkit_agent_policy::PermissionDecision::Deny
        }
    };
    log.append(
        timestamp_ms,
        decision,
        format!("credential_request:{}", outcome.as_str()),
        "credential_request",
        vec![
            ("tool_call_id".to_string(), request.tool_call_id.clone()),
            (
                "requested_for".to_string(),
                request.kind.label().to_string(),
            ),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_value_never_prints_inner_bytes() {
        let secret = SecretValue::new(concat!("sk", "-super-secret-token"));
        assert_eq!(format!("{secret:?}"), "SecretValue(<redacted>)");
        assert_eq!(format!("{secret}"), "<redacted>");
        assert_eq!(secret.expose(), concat!("sk", "-super-secret-token"));
    }

    #[test]
    fn credential_resume_debug_never_prints_supplied_value() {
        let resume = CredentialResume::Supplied(SecretValue::new("hunter2"));
        assert_eq!(
            format!("{resume:?}"),
            "CredentialResume::Supplied(<redacted>)"
        );
    }

    fn sample_request(tool_call_id: &str) -> CredentialRequest {
        CredentialRequest::new(tool_call_id, CredentialKind::Cloudflare, "needs a token")
    }

    #[tokio::test]
    async fn resolve_delivers_supplied_value_to_waiting_receiver() {
        let mut registry = PendingCredentialRequests::new();
        let rx = registry.insert(sample_request("call-1"));
        assert!(registry.take_and_resolve(
            "call-1",
            CredentialResume::Supplied(SecretValue::new("token-abc"))
        ));
        match rx.await.expect("resume should deliver") {
            CredentialResume::Supplied(value) => assert_eq!(value.expose(), "token-abc"),
            CredentialResume::Declined => panic!("expected Supplied"),
        }
    }

    #[tokio::test]
    async fn resolve_is_single_use() {
        let mut registry = PendingCredentialRequests::new();
        let _rx = registry.insert(sample_request("call-1"));
        assert!(registry.take_and_resolve("call-1", CredentialResume::Declined));
        // Second resolve attempt for the same call id finds nothing: the
        // entry was removed by the first take_and_resolve, so this cannot
        // redeliver or replay a credential into a second consumer.
        assert!(!registry.take_and_resolve("call-1", CredentialResume::Declined));
    }

    #[tokio::test]
    async fn inserting_a_new_request_for_the_same_call_supersedes_the_old_receiver() {
        let mut registry = PendingCredentialRequests::new();
        let stale_rx = registry.insert(sample_request("call-1"));
        let _fresh_rx = registry.insert(sample_request("call-1"));
        // The stale receiver's sender was dropped by the second insert, so
        // it can never observe a resolve meant for the fresh request.
        assert!(stale_rx.await.is_err());
    }

    #[tokio::test]
    async fn remove_drops_pending_request_without_resolving() {
        let mut registry = PendingCredentialRequests::new();
        let rx = registry.insert(sample_request("call-1"));
        registry.remove("call-1");
        assert!(rx.await.is_err());
        assert!(registry.is_empty());
    }

    #[tokio::test]
    async fn pending_lists_without_consuming() {
        let mut registry = PendingCredentialRequests::new();
        let _rx = registry.insert(sample_request("call-1"));
        // Listing twice must not consume or invalidate the entry.
        assert_eq!(registry.pending().len(), 1);
        assert_eq!(registry.pending().len(), 1);
        assert!(registry.take_and_resolve("call-1", CredentialResume::Declined));
        assert!(registry.pending().is_empty());
    }

    #[test]
    fn decision_log_never_carries_the_supplied_secret() {
        let mut log = rightkit_agent_policy::decision_log::DecisionLog::new();
        let request = CredentialRequest::new(
            "call-1",
            CredentialKind::Cloudflare,
            "deploy_worker needs an account API token",
        );
        record_requested(&mut log, 1_000, &request);
        record_outcome(&mut log, 1_001, &request, CredentialOutcome::Supplied);

        assert_eq!(log.len(), 2);
        for entry in log.entries() {
            for (key, value) in &entry.context {
                assert_ne!(key, "value");
                assert_ne!(value, "token-abc");
                assert_ne!(value, concat!("sk", "-super-secret-token"));
            }
        }
        // The non-secret credential label is retained (proves the log isn't
        // over-scrubbing the harmless identity fields either).
        let requested_for: Vec<&str> = log
            .entries()
            .iter()
            .flat_map(|e| e.context.iter())
            .filter(|(k, _)| k == "requested_for")
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(requested_for, vec!["cloudflare", "cloudflare"]);
    }

    #[test]
    fn other_kind_carries_arbitrary_non_secret_label() {
        let kind = CredentialKind::Other("supabase".to_string());
        assert_eq!(kind.label(), "supabase");
    }
}
