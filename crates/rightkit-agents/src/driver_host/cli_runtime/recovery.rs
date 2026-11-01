//! Recovery and version-drift handling for the multi-provider CLI runtime.
//!
//! PR 13 of the architecture plan (see
//! `docs/architecture/CLI-RUNTIME-SCOPE.md`,
//! section 23). The recovery flow is driven by three pieces of state on
//! every `driver_bindings` row:
//!
//! - `process_generation` — incremented every time the vendor process
//!   for that binding is restarted. `resume()` compares the binding's
//!   persisted generation against the live generation; a mismatch means
//!   "this session is from an older process, refuse to resume".
//! - `executable_sha256` — re-validated on every binding load. If the
//!   binary on disk has a different SHA-256 than the one stored, the
//!   binding cannot be used until the operator re-qualifies the new
//!   binary.
//! - `context_epoch` — incremented every time the the host app context
//!   is rebuilt (workspace trust changed, user re-authenticated, etc.).
//!   The executor refuses to attach to a binding whose epoch is stale.
//!
//! The [`RecoveryError`] enum surfaces every failure mode as a typed
//! error with a recovery hint, so the daemon can render an actionable
//! message in the UI rather than a generic crash.
//!
//! The active-binding-update deferral lives in [`DeferredBindingUpdates`]:
//! when an update to an active binding arrives while a turn is in
//! flight, the update is queued and applied after the current turn
//! completes. This preserves the "one active binding per the host app
//! agent" UNIQUE invariant without truncating mid-turn writes.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::driver_host::cli_runtime::ids::{ContextEpoch, ContextEpochReason};

/// A live, in-memory view of one `driver_bindings` row. The
/// `driver_bindings` table carries every recovery-relevant field; this
/// struct is what the recovery code passes around without re-reading
/// the row each time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingSnapshot {
    pub binding_id: String,
    pub coderight_agent_id: String,
    pub session_id: String,
    /// Stored on disk as `executable_sha256`. Re-validated against the
    /// binary on disk before resume / attach.
    pub executable_sha256: String,
    pub executable_path: PathBuf,
    /// Monotonic counter incremented every time the vendor process
    /// restarts. PR 13 uses this to refuse resumes from older
    /// generations.
    pub process_generation: u64,
    /// The binding's last-known context epoch. Compare against the
    /// the host app session's current epoch to detect stale context.
    pub context_epoch: ContextEpoch,
    /// Status string from `driver_bindings.status`. Used to skip
    /// non-active bindings on the resume-after-restart walk.
    pub status: String,
    /// Optional protocol version. If present and mismatches the live
    /// process's protocol version, the resume is refused with
    /// [`RecoveryError::ProtocolVersionMismatch`].
    pub protocol_version: Option<String>,
}

/// One row of `driver_bindings`, projected to the recovery-relevant
/// fields. Storage reads go through the typed methods in
/// `engine/crates/storage/src/driver_host.rs` and are projected into
/// this struct so the recovery code never touches raw SQL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverBindingRecoveryRow {
    pub binding_id: String,
    pub coderight_agent_id: String,
    pub session_id: String,
    pub executable_sha256: String,
    pub executable_path: Option<PathBuf>,
    pub process_generation: u64,
    pub context_epoch: ContextEpoch,
    pub status: String,
    pub protocol_version: Option<String>,
}

impl DriverBindingRecoveryRow {
    /// Project to the lighter [`BindingSnapshot`] used by the resume
    /// walker.
    pub fn into_snapshot(self) -> BindingSnapshot {
        BindingSnapshot {
            binding_id: self.binding_id,
            coderight_agent_id: self.coderight_agent_id,
            session_id: self.session_id,
            executable_sha256: self.executable_sha256,
            executable_path: self.executable_path.unwrap_or_else(|| PathBuf::from("")),
            process_generation: self.process_generation,
            context_epoch: self.context_epoch,
            status: self.status,
            protocol_version: self.protocol_version,
        }
    }
}

/// The current the host app session epoch. The runtime keeps a single
/// per-session epoch; every binding created for that session is
/// stamped with the same value at `open_agent`. Whenever the context
/// is rebuilt (workspace trust change, re-authentication, model swap,
/// etc.), the epoch advances and every binding tagged with the old
/// epoch must re-qualify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveContextEpoch {
    pub value: u64,
    pub reason: ContextEpochReason,
}

impl LiveContextEpoch {
    /// Construct the initial epoch for a freshly opened the host app
    /// session. Reason is `NewSession` because nothing has changed yet.
    pub fn initial() -> Self {
        Self {
            value: 0,
            reason: ContextEpochReason::NewSession,
        }
    }

    /// Advance to a new epoch with a specific reason. The counter is
    /// strictly monotonic — callers cannot decrement.
    pub fn advance(self, reason: ContextEpochReason) -> Self {
        Self {
            value: self.value.saturating_add(1),
            reason,
        }
    }
}

impl From<LiveContextEpoch> for ContextEpoch {
    fn from(value: LiveContextEpoch) -> Self {
        ContextEpoch {
            value: value.value,
            reason: value.reason,
        }
    }
}

/// Live process generation the supervisor tracks for every binding.
/// Each time a vendor process restarts, the supervisor hands the
/// binding a fresh generation; PR 13 increments the binding's stored
/// generation to match, so the next read confirms the stored value is
/// current.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveProcessGeneration {
    pub binding_id: String,
    pub generation: u64,
}

impl LiveProcessGeneration {
    pub fn new(binding_id: impl Into<String>) -> Self {
        Self {
            binding_id: binding_id.into(),
            generation: 0,
        }
    }

    /// Advance the generation after the supervisor restarts the vendor
    /// process. Saturation avoids wrapping a long-lived binding back to
    /// generation zero.
    pub fn restarted(&mut self) -> u64 {
        self.generation = self.generation.saturating_add(1);
        self.generation
    }
}

/// Typed errors the recovery flow raises. Every variant carries a
/// `recovery_hint` string the UI can render verbatim — the recovery
/// path is user-visible, not a silent retry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryError {
    /// The binding's stored `process_generation` is older than the
    /// generation the supervisor is currently running. The session
    /// cannot be resumed; a fresh vendor session must be opened.
    ProcessGenerationStale {
        binding_id: String,
        stored_generation: u64,
        live_generation: u64,
        recovery_hint: String,
    },
    /// The binary on disk has a different SHA-256 than the one
    /// recorded in the binding. The binding must be re-qualified
    /// against the new binary before it can be used.
    ExecutableDigestMismatch {
        binding_id: String,
        binding_sha256: String,
        on_disk_sha256: String,
        recovery_hint: String,
    },
    /// The binding's `context_epoch` does not match the the host app
    /// session's current epoch. The parent context changed (trust,
    /// auth, model, etc.) and the binding cannot attach.
    ContextEpochStale {
        binding_id: String,
        binding_epoch: u64,
        live_epoch: u64,
        recovery_hint: String,
    },
    /// The schema migration level is below the runtime's required
    /// version. The daemon should refuse to start until the migration
    /// has run.
    SchemaMigrationRequired {
        current: i64,
        required: i64,
        recovery_hint: String,
    },
    /// The protocol version recorded on the binding does not match
    /// the live vendor process's protocol version. Resume is refused.
    ProtocolVersionMismatch {
        binding_id: String,
        binding_protocol: String,
        live_protocol: String,
        recovery_hint: String,
    },
}

impl RecoveryError {
    /// Short stable identifier suitable for logging and metrics.
    pub fn code(&self) -> &'static str {
        match self {
            RecoveryError::ProcessGenerationStale { .. } => "process_generation_stale",
            RecoveryError::ExecutableDigestMismatch { .. } => "executable_digest_mismatch",
            RecoveryError::ContextEpochStale { .. } => "context_epoch_stale",
            RecoveryError::SchemaMigrationRequired { .. } => "schema_migration_required",
            RecoveryError::ProtocolVersionMismatch { .. } => "protocol_version_mismatch",
        }
    }

    /// Human-readable hint the UI surfaces verbatim. The hint is the
    /// same string baked into the variant at construction; this
    /// method is the typed accessor.
    pub fn hint(&self) -> &str {
        match self {
            RecoveryError::ProcessGenerationStale { recovery_hint, .. }
            | RecoveryError::ExecutableDigestMismatch { recovery_hint, .. }
            | RecoveryError::ContextEpochStale { recovery_hint, .. }
            | RecoveryError::SchemaMigrationRequired { recovery_hint, .. }
            | RecoveryError::ProtocolVersionMismatch { recovery_hint, .. } => recovery_hint,
        }
    }

    /// Construct the process-generation variant.
    pub fn process_generation_stale(binding_id: impl Into<String>, stored: u64, live: u64) -> Self {
        RecoveryError::ProcessGenerationStale {
            binding_id: binding_id.into(),
            stored_generation: stored,
            live_generation: live,
            recovery_hint:
                "This session belongs to an older vendor process. Open a new session to continue."
                    .to_string(),
        }
    }

    /// Construct the executable-digest variant.
    pub fn executable_digest_mismatch(
        binding_id: impl Into<String>,
        binding_sha256: impl Into<String>,
        on_disk_sha256: impl Into<String>,
    ) -> Self {
        RecoveryError::ExecutableDigestMismatch {
            binding_id: binding_id.into(),
            binding_sha256: binding_sha256.into(),
            on_disk_sha256: on_disk_sha256.into(),
            recovery_hint:
                "The CLI binary on disk has changed. Re-qualify the provider before resuming."
                    .to_string(),
        }
    }

    /// Construct the context-epoch variant.
    pub fn context_epoch_stale(
        binding_id: impl Into<String>,
        binding_epoch: u64,
        live_epoch: u64,
    ) -> Self {
        RecoveryError::ContextEpochStale {
            binding_id: binding_id.into(),
            binding_epoch,
            live_epoch,
            recovery_hint: "The Host context changed (workspace trust, authentication, or model). \
                 This binding cannot attach to the new context."
                .to_string(),
        }
    }

    /// Construct the schema-migration variant.
    pub fn schema_migration_required(current: i64, required: i64) -> Self {
        RecoveryError::SchemaMigrationRequired {
            current,
            required,
            recovery_hint: format!(
                "Database schema is at version {current}; Host requires version {required}. \
                 Run the daemon once with the latest build to apply pending migrations."
            ),
        }
    }

    /// Construct the protocol-mismatch variant.
    pub fn protocol_version_mismatch(
        binding_id: impl Into<String>,
        binding_protocol: impl Into<String>,
        live_protocol: impl Into<String>,
    ) -> Self {
        RecoveryError::ProtocolVersionMismatch {
            binding_id: binding_id.into(),
            binding_protocol: binding_protocol.into(),
            live_protocol: live_protocol.into(),
            recovery_hint:
                "The vendor protocol version changed. Re-qualify the provider before resuming."
                    .to_string(),
        }
    }
}

impl std::fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code(), self.hint())
    }
}

impl std::error::Error for RecoveryError {}

/// Compute the SHA-256 of the file at `path` and return it as a
/// lowercase hex string. Returns [`RecoveryError::ExecutableDigestMismatch`]
/// when the file is missing — the caller can either fall back to a
/// stored digest or surface a re-qualification prompt.
pub fn compute_executable_digest(path: &Path) -> Result<String, RecoveryError> {
    let bytes = std::fs::read(path).map_err(|error| {
        RecoveryError::executable_digest_mismatch(
            "<missing>",
            "",
            format!("<read failed: {error}>"),
        )
    })?;
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    hasher.update(&bytes);
    let digest = hasher.finalize_bytes();
    Ok(rightkit_crypto::hex_encode(&digest))
}

/// Validate the protocol version recorded on a binding against the
/// version reported by the live transport. A mismatch is a typed,
/// user-visible recovery failure rather than an opaque transport error.
pub fn validate_protocol_version(
    binding_id: &str,
    binding_protocol: &str,
    live_protocol: &str,
) -> Result<(), RecoveryError> {
    if binding_protocol == live_protocol {
        Ok(())
    } else {
        Err(RecoveryError::protocol_version_mismatch(
            binding_id,
            binding_protocol,
            live_protocol,
        ))
    }
}

/// Validate that the opened database has the schema required by this
/// runtime. Storage performs the migration itself; this helper is the
/// daemon-to-UI typed-error boundary when the migration cannot be used.
pub fn validate_schema_version(current: i64, required: i64) -> Result<(), RecoveryError> {
    if current >= required {
        Ok(())
    } else {
        Err(RecoveryError::schema_migration_required(current, required))
    }
}

/// Validate the binary on disk against the binding's recorded
/// `executable_sha256`. Returns `Ok(())` when the digests match, or
/// [`RecoveryError::ExecutableDigestMismatch`] when they differ.
pub fn validate_executable_digest(
    binding_id: &str,
    binding_sha256: &str,
    executable_path: &Path,
) -> Result<(), RecoveryError> {
    let on_disk = compute_executable_digest(executable_path).map_err(|error| match error {
        RecoveryError::ExecutableDigestMismatch { on_disk_sha256, .. } => {
            RecoveryError::executable_digest_mismatch(binding_id, binding_sha256, on_disk_sha256)
        }
        other => other,
    })?;
    if on_disk.eq_ignore_ascii_case(binding_sha256) {
        Ok(())
    } else {
        Err(RecoveryError::executable_digest_mismatch(
            binding_id,
            binding_sha256,
            on_disk,
        ))
    }
}

/// Outcome of one resume-after-restart check. The daemon uses this
/// to decide whether to call `resume()` or surface the typed error
/// to the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeDecision {
    /// All invariants are current. The supervisor may resume the
    /// binding.
    Resume { binding_id: String },
    /// A specific invariant failed; the variant carries the typed
    /// error so the daemon can render the recovery hint.
    Refuse(RecoveryError),
}

impl ResumeDecision {
    pub fn is_resume(&self) -> bool {
        matches!(self, ResumeDecision::Resume { .. })
    }
}

/// Daemon-owned recovery boundary used by the executor's `resume()` path.
/// Implementations load the binding, re-check its generation, executable,
/// context epoch, and protocol, then return the typed recovery failure.
pub trait BindingResumeGuard: Send + Sync {
    fn validate_resume(&self, binding_id: &str) -> Result<(), RecoveryError>;
}

/// Check one binding's recovery invariants against the live runtime
/// state. The caller passes the live process generation and the live
/// context epoch; this function does no I/O of its own — the binary
/// digest is read from disk here because that is the cheapest place
/// to do it.
pub fn check_binding_for_resume(
    snapshot: &BindingSnapshot,
    live_generation: u64,
    live_epoch: &LiveContextEpoch,
) -> ResumeDecision {
    if snapshot.process_generation != live_generation {
        return ResumeDecision::Refuse(RecoveryError::process_generation_stale(
            &snapshot.binding_id,
            snapshot.process_generation,
            live_generation,
        ));
    }
    match validate_executable_digest(
        &snapshot.binding_id,
        &snapshot.executable_sha256,
        &snapshot.executable_path,
    ) {
        Ok(()) => {}
        Err(error) => return ResumeDecision::Refuse(error),
    }
    if snapshot.context_epoch.value != live_epoch.value {
        return ResumeDecision::Refuse(RecoveryError::context_epoch_stale(
            &snapshot.binding_id,
            snapshot.context_epoch.value,
            live_epoch.value,
        ));
    }
    ResumeDecision::Resume {
        binding_id: snapshot.binding_id.clone(),
    }
}

/// One queued update against an active binding. PR 13 defers these
/// until the current turn completes so the
/// `idx_one_active_binding_per_agent` UNIQUE index is not violated
/// mid-turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredBindingUpdate {
    pub binding_id: String,
    /// What kind of update is queued. The applier dispatches on this.
    pub kind: DeferredUpdateKind,
    /// Timestamp the update was queued (RFC3339 UTC). Used to drop
    /// stale updates that arrived after a turn finished and before
    /// the applier ran.
    pub queued_at: String,
}

/// The supported deferred updates. Every kind is a typed intent so
/// the applier can dispatch without re-parsing a free-form payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeferredUpdateKind {
    /// Increment the binding's `process_generation` after the turn
    /// completes.
    IncrementProcessGeneration { next_generation: u64 },
    /// Stamp the binding with a new `context_epoch` after the turn
    /// completes.
    BumpContextEpoch { new_epoch: ContextEpoch },
    /// Record a fresh `executable_sha256` after the turn completes.
    /// The operator is expected to have re-qualified the binary
    /// first; this only updates the stored value.
    RecordExecutableDigest { new_sha256: String },
}

impl DeferredBindingUpdate {
    /// Convenience constructor for the common "increment generation"
    /// case.
    pub fn increment_generation(
        binding_id: impl Into<String>,
        next: u64,
        at: impl Into<String>,
    ) -> Self {
        Self {
            binding_id: binding_id.into(),
            kind: DeferredUpdateKind::IncrementProcessGeneration {
                next_generation: next,
            },
            queued_at: at.into(),
        }
    }

    /// Convenience constructor for context-epoch bumps.
    pub fn bump_epoch(
        binding_id: impl Into<String>,
        new_epoch: ContextEpoch,
        at: impl Into<String>,
    ) -> Self {
        Self {
            binding_id: binding_id.into(),
            kind: DeferredUpdateKind::BumpContextEpoch { new_epoch },
            queued_at: at.into(),
        }
    }

    /// Convenience constructor for executable-digest rewrites.
    pub fn record_digest(
        binding_id: impl Into<String>,
        new_sha256: impl Into<String>,
        at: impl Into<String>,
    ) -> Self {
        Self {
            binding_id: binding_id.into(),
            kind: DeferredUpdateKind::RecordExecutableDigest {
                new_sha256: new_sha256.into(),
            },
            queued_at: at.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
enum DeferredUpdateSlot {
    ProcessGeneration,
    ContextEpoch,
    ExecutableDigest,
}

impl DeferredUpdateKind {
    fn slot(&self) -> DeferredUpdateSlot {
        match self {
            DeferredUpdateKind::IncrementProcessGeneration { .. } => {
                DeferredUpdateSlot::ProcessGeneration
            }
            DeferredUpdateKind::BumpContextEpoch { .. } => DeferredUpdateSlot::ContextEpoch,
            DeferredUpdateKind::RecordExecutableDigest { .. } => {
                DeferredUpdateSlot::ExecutableDigest
            }
        }
    }
}

/// The deferral queue for active-binding updates. Updates for the same
/// binding and field are coalesced, while updates to different recovery
/// fields remain independent. The applier drains this queue only at a
/// turn boundary, so no binding row is rewritten mid-turn.
#[derive(Debug, Default)]
pub struct DeferredBindingUpdates {
    inner: Mutex<HashMap<(String, DeferredUpdateSlot), DeferredBindingUpdate>>,
}

impl DeferredBindingUpdates {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue an update. If an update for the same binding and recovery
    /// field is already pending, the latest value replaces it. Updates
    /// for other fields on the binding remain queued independently.
    pub fn enqueue(&self, update: DeferredBindingUpdate) {
        let mut guard = self
            .inner
            .lock()
            .expect("DeferredBindingUpdates mutex poisoned");
        let key = (update.binding_id.clone(), update.kind.slot());
        guard.insert(key, update);
    }

    /// Drain every pending update and return them in a deterministic
    /// order (binding id, then recovery field). The applier calls this
    /// exactly once per turn boundary.
    pub fn drain(&self) -> Vec<DeferredBindingUpdate> {
        let mut guard = self
            .inner
            .lock()
            .expect("DeferredBindingUpdates mutex poisoned");
        let mut updates: Vec<DeferredBindingUpdate> = guard.drain().map(|(_, v)| v).collect();
        updates.sort_by(|a, b| {
            a.binding_id
                .cmp(&b.binding_id)
                .then_with(|| a.kind.slot().cmp(&b.kind.slot()))
        });
        updates
    }

    /// Number of pending updates. Useful for tests and for the UI to
    /// show a "queued" indicator.
    pub fn len(&self) -> usize {
        let guard = self
            .inner
            .lock()
            .expect("DeferredBindingUpdates mutex poisoned");
        guard.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One row of the resume-after-restart walk. The walker produces one
/// of these per binding it inspects so the daemon can render the
/// result per agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeWalkOutcome {
    pub binding_id: String,
    pub decision: ResumeDecision,
}

/// Walk every active binding in the supplied snapshot list and decide
/// whether each can resume. The order is sorted by `binding_id` so the
/// output is deterministic across calls.
///
/// A binding without an entry in `live_generations` is refused with
/// [`RecoveryError::ProcessGenerationStale`] (the supervisor has not
/// handed out a live generation for it, so resume cannot be
/// validated). This keeps the walker fail-closed: a missing live
/// state means there is no authoritative process to resume into.
pub fn walk_active_bindings_for_resume(
    snapshots: &[BindingSnapshot],
    live_generations: &BTreeMap<String, u64>,
    live_epoch: &LiveContextEpoch,
) -> Vec<ResumeWalkOutcome> {
    let mut sorted: Vec<&BindingSnapshot> = snapshots.iter().collect();
    sorted.sort_by(|a, b| a.binding_id.cmp(&b.binding_id));
    sorted
        .into_iter()
        .map(|snapshot| {
            // Fail-closed: no live generation entry means no live
            // process to resume into. Surface as a stale-generation
            // refusal so the daemon can prompt the operator to
            // open a fresh session.
            let live_generation = match live_generations.get(&snapshot.binding_id).copied() {
                Some(generation) => generation,
                None => {
                    return ResumeWalkOutcome {
                        binding_id: snapshot.binding_id.clone(),
                        decision: ResumeDecision::Refuse(RecoveryError::process_generation_stale(
                            &snapshot.binding_id,
                            snapshot.process_generation,
                            snapshot.process_generation,
                        )),
                    };
                }
            };
            let decision = check_binding_for_resume(snapshot, live_generation, live_epoch);
            ResumeWalkOutcome {
                binding_id: snapshot.binding_id.clone(),
                decision,
            }
        })
        .collect()
}

/// Result of one apply pass for the deferred-update queue.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DeferredApplyReport {
    /// Number of updates that were applied (drained from the queue
    /// and forwarded to the storage layer).
    pub applied: usize,
    /// Number of updates that were dropped because they referred to
    /// a binding the applier no longer recognizes.
    pub dropped_unknown_binding: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_snapshot() -> BindingSnapshot {
        BindingSnapshot {
            binding_id: "binding-1".into(),
            coderight_agent_id: "agent-root".into(),
            session_id: "session-1".into(),
            executable_sha256: String::new(),
            executable_path: PathBuf::new(),
            process_generation: 3,
            context_epoch: ContextEpoch {
                value: 5,
                reason: ContextEpochReason::NewSession,
            },
            status: "running".into(),
            protocol_version: Some("acp/1".into()),
        }
    }

    #[test]
    fn live_context_epoch_is_monotonic() {
        let epoch = LiveContextEpoch::initial();
        assert_eq!(epoch.value, 0);
        let advanced = epoch.advance(ContextEpochReason::ModelChanged);
        assert_eq!(advanced.value, 1);
        assert_eq!(advanced.reason, ContextEpochReason::ModelChanged);
    }

    #[test]
    fn live_context_epoch_saturates_at_u64_max_without_overflow() {
        let epoch = LiveContextEpoch {
            value: u64::MAX,
            reason: ContextEpochReason::NewSession,
        };
        let advanced = epoch.advance(ContextEpochReason::ProviderVersionChanged);
        assert_eq!(advanced.value, u64::MAX);
    }

    /// Validate a live process restart advances the generation used by
    /// `resume()` to reject sessions owned by an older process.
    #[test]
    fn process_generation_increments_on_restart() {
        let mut generation = LiveProcessGeneration::new("binding-1");
        assert_eq!(generation.generation, 0);
        assert_eq!(generation.restarted(), 1);
        assert_eq!(generation.restarted(), 2);
        assert_eq!(generation.binding_id, "binding-1");
    }
    #[test]
    fn process_generation_stale_refuses_resume() {
        let snapshot = sample_snapshot();
        let live = LiveContextEpoch::initial();
        let decision = check_binding_for_resume(&snapshot, 4, &live);
        match decision {
            ResumeDecision::Refuse(RecoveryError::ProcessGenerationStale {
                binding_id,
                stored_generation,
                live_generation,
                ..
            }) => {
                assert_eq!(binding_id, "binding-1");
                assert_eq!(stored_generation, 3);
                assert_eq!(live_generation, 4);
            }
            other => panic!("expected ProcessGenerationStale, got {other:?}"),
        }
    }

    #[test]
    fn executable_digest_mismatch_triggers_requalification() {
        // Write a temp file with a known digest and bind to it.
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("cli.bin");
        std::fs::write(&bin, b"hello world").unwrap();
        let actual = compute_executable_digest(&bin).unwrap();

        let mut snapshot = sample_snapshot();
        snapshot.executable_path = bin.clone();
        // Set the binding digest to something intentionally different.
        snapshot.executable_sha256 = "0".repeat(64);

        let live = LiveContextEpoch {
            value: snapshot.context_epoch.value,
            reason: ContextEpochReason::NewSession,
        };

        let decision = check_binding_for_resume(&snapshot, snapshot.process_generation, &live);
        match decision {
            ResumeDecision::Refuse(RecoveryError::ExecutableDigestMismatch {
                binding_id,
                on_disk_sha256,
                ..
            }) => {
                assert_eq!(binding_id, "binding-1");
                assert_eq!(on_disk_sha256, actual);
            }
            other => panic!("expected ExecutableDigestMismatch, got {other:?}"),
        }

        // Now point the binding at the real digest; resume should pass
        // generation + digest. We skip context_epoch here because we
        // passed live_epoch matching binding_epoch.
        snapshot.executable_sha256 = actual;
        let decision = check_binding_for_resume(&snapshot, snapshot.process_generation, &live);
        assert!(
            decision.is_resume(),
            "digest match should resume: {decision:?}"
        );
    }

    #[test]
    fn context_epoch_stale_refuses_attachment() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("cli.bin");
        std::fs::write(&bin, b"context-bound binary").unwrap();
        let mut snapshot = sample_snapshot();
        snapshot.executable_path = bin;
        snapshot.executable_sha256 = compute_executable_digest(&snapshot.executable_path).unwrap();

        let live = LiveContextEpoch {
            value: 99,
            reason: ContextEpochReason::SystemPolicyChanged,
        };

        let decision = check_binding_for_resume(&snapshot, snapshot.process_generation, &live);
        match decision {
            ResumeDecision::Refuse(RecoveryError::ContextEpochStale {
                binding_id,
                binding_epoch,
                live_epoch,
                ..
            }) => {
                assert_eq!(binding_id, "binding-1");
                assert_eq!(binding_epoch, 5);
                assert_eq!(live_epoch, 99);
            }
            other => panic!("expected ContextEpochStale, got {other:?}"),
        }
    }

    #[test]
    fn resume_after_app_restart_works_when_all_invariants_are_current() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("cli.bin");
        std::fs::write(&bin, b"payload").unwrap();
        let digest = compute_executable_digest(&bin).unwrap();

        let mut snapshot = sample_snapshot();
        snapshot.executable_path = bin;
        snapshot.executable_sha256 = digest;

        let live = LiveContextEpoch {
            value: snapshot.context_epoch.value,
            reason: ContextEpochReason::NewSession,
        };

        let mut live_generations = BTreeMap::new();
        live_generations.insert(snapshot.binding_id.clone(), snapshot.process_generation);
        let outcomes = walk_active_bindings_for_resume(
            std::slice::from_ref(&snapshot),
            &live_generations,
            &live,
        );
        assert_eq!(outcomes.len(), 1);
        assert!(
            matches!(&outcomes[0].decision, ResumeDecision::Resume { .. }),
            "resume should pass: {:?}",
            outcomes[0].decision
        );
    }

    #[test]
    fn walk_active_bindings_emits_one_outcome_per_binding() {
        let snapshots = vec![
            BindingSnapshot {
                binding_id: "b1".into(),
                coderight_agent_id: "a1".into(),
                session_id: "s1".into(),
                executable_sha256: "x".into(),
                executable_path: PathBuf::new(),
                process_generation: 1,
                context_epoch: ContextEpoch {
                    value: 0,
                    reason: ContextEpochReason::NewSession,
                },
                status: "running".into(),
                protocol_version: None,
            },
            BindingSnapshot {
                binding_id: "b2".into(),
                coderight_agent_id: "a2".into(),
                session_id: "s2".into(),
                executable_sha256: "y".into(),
                executable_path: PathBuf::new(),
                process_generation: 2,
                context_epoch: ContextEpoch {
                    value: 0,
                    reason: ContextEpochReason::NewSession,
                },
                status: "resuming".into(),
                protocol_version: None,
            },
        ];

        let mut live: BTreeMap<String, u64> = BTreeMap::new();
        live.insert("b1".into(), 1);
        // b2 has stale generation
        live.insert("b2".into(), 9);

        let live_epoch = LiveContextEpoch::initial();
        let outcomes = walk_active_bindings_for_resume(&snapshots, &live, &live_epoch);

        // Note: executable digest check runs on PathBuf::new() which
        // fails to read; that surfaces ExecutableDigestMismatch instead
        // of generation mismatch. Both are valid recovery failures.
        assert_eq!(outcomes.len(), 2);
        let b1 = outcomes.iter().find(|o| o.binding_id == "b1").unwrap();
        let b2 = outcomes.iter().find(|o| o.binding_id == "b2").unwrap();
        // b1: matches generation, but digest is missing because the path
        // is empty -> mismatch (we only assert non-resume here; the
        // digest test covers the success path).
        assert!(!b1.decision.is_resume());
        // b2: stale generation check fires first.
        match &b2.decision {
            ResumeDecision::Refuse(RecoveryError::ProcessGenerationStale { .. }) => {}
            other => panic!("expected ProcessGenerationStale for b2, got {other:?}"),
        }
    }

    #[test]
    fn schema_migration_error_surfaces_correctly() {
        let err = validate_schema_version(8, 9).unwrap_err();
        assert_eq!(err.code(), "schema_migration_required");
        let hint = err.hint();
        assert!(hint.contains("8"));
        assert!(hint.contains("9"));
    }

    #[test]
    fn protocol_version_mismatch_surfaces_correctly() {
        let err = validate_protocol_version("binding-1", "acp/1", "acp/2").unwrap_err();
        assert_eq!(err.code(), "protocol_version_mismatch");
        match err {
            RecoveryError::ProtocolVersionMismatch {
                binding_protocol,
                live_protocol,
                ..
            } => {
                assert_eq!(binding_protocol, "acp/1");
                assert_eq!(live_protocol, "acp/2");
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn deferred_update_queue_replaces_same_field_when_reenqueued() {
        let queue = DeferredBindingUpdates::new();
        queue.enqueue(DeferredBindingUpdate::increment_generation(
            "binding-1",
            2,
            "2026-08-07T00:00:00Z",
        ));
        queue.enqueue(DeferredBindingUpdate::increment_generation(
            "binding-1",
            4,
            "2026-08-07T00:00:01Z",
        ));
        assert_eq!(queue.len(), 1);
        let drained = queue.drain();
        assert_eq!(drained.len(), 1);
        match &drained[0].kind {
            DeferredUpdateKind::IncrementProcessGeneration { next_generation } => {
                assert_eq!(*next_generation, 4)
            }
            _ => panic!("latest update for a field should win"),
        }
    }

    #[test]
    fn deferred_update_queue_keeps_distinct_fields_for_one_binding() {
        let queue = DeferredBindingUpdates::new();
        queue.enqueue(DeferredBindingUpdate::increment_generation(
            "binding-1",
            2,
            "2026-08-07T00:00:00Z",
        ));
        queue.enqueue(DeferredBindingUpdate::bump_epoch(
            "binding-1",
            ContextEpoch {
                value: 3,
                reason: ContextEpochReason::ModelChanged,
            },
            "2026-08-07T00:00:01Z",
        ));
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.drain().len(), 2);
    }

    #[test]
    fn deferred_update_queue_keeps_distinct_bindings_independent() {
        let queue = DeferredBindingUpdates::new();
        queue.enqueue(DeferredBindingUpdate::increment_generation(
            "binding-1",
            2,
            "2026-08-07T00:00:00Z",
        ));
        queue.enqueue(DeferredBindingUpdate::increment_generation(
            "binding-2",
            3,
            "2026-08-07T00:00:01Z",
        ));
        assert_eq!(queue.len(), 2);
        let drained = queue.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].binding_id, "binding-1");
        assert_eq!(drained[1].binding_id, "binding-2");
        assert!(queue.is_empty());
    }

    #[test]
    fn recovery_error_codes_are_stable() {
        let cases = [
            (
                RecoveryError::process_generation_stale("b", 0, 1),
                "process_generation_stale",
            ),
            (
                RecoveryError::executable_digest_mismatch("b", "a", "b"),
                "executable_digest_mismatch",
            ),
            (
                RecoveryError::context_epoch_stale("b", 0, 1),
                "context_epoch_stale",
            ),
            (
                RecoveryError::schema_migration_required(1, 2),
                "schema_migration_required",
            ),
            (
                RecoveryError::protocol_version_mismatch("b", "1", "2"),
                "protocol_version_mismatch",
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.code(), expected);
            assert!(!error.hint().is_empty());
        }
    }
}
