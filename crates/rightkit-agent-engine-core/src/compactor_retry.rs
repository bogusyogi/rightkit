// Mechanism 7 (book §P2-3, 3-7): monotonic retry.
//
// Sibling include! module of compactor.rs -- shares its module scope, no
// mod/use headers of its own. The book's cited prior art, deepseek-harness,
// is now present at `repos/deepseek-harness`, but no matching monotonic-retry
// source was located there (#70); this remains implemented from the book's
// prose description, not read from source.

use serde::Deserialize;

/// Failure-relevant state captured before summarization/federation/storage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionSnapshotV1 {
    pub request_generation: u64,
    pub transcript_high_water: i64,
    pub transcript_digest: String,
    pub pin_revision: Option<u64>,
    pub model_route_revision: Option<u64>,
    pub tool_catalog_revision: Option<u64>,
    pub promotion_revision: Option<u64>,
    pub capacity_revision: Option<u64>,
    pub reserve_revision: Option<u64>,
    pub accounting_revision: Option<u64>,
    pub federation_revision: Option<u64>,
    pub binding_revision: Option<u64>,
    pub h9_revision: Option<u64>,
    pub policy_revision: Option<u64>,
    pub repository_scope_revision: Option<u64>,
    pub decision_expires_at_unix_ms: u64,
}

impl CompactionSnapshotV1 {
    pub fn failure_relevant_eq(&self, other: &Self) -> bool {
        self.request_generation == other.request_generation
            && self.transcript_high_water == other.transcript_high_water
            && self.transcript_digest == other.transcript_digest
            && self.pin_revision == other.pin_revision
            && self.model_route_revision == other.model_route_revision
            && self.tool_catalog_revision == other.tool_catalog_revision
            && self.promotion_revision == other.promotion_revision
            && self.capacity_revision == other.capacity_revision
            && self.reserve_revision == other.reserve_revision
            && self.accounting_revision == other.accounting_revision
            && self.federation_revision == other.federation_revision
            && self.binding_revision == other.binding_revision
            && self.h9_revision == other.h9_revision
            && self.policy_revision == other.policy_revision
            && self.repository_scope_revision == other.repository_scope_revision
            && self.decision_expires_at_unix_ms == other.decision_expires_at_unix_ms
    }

    pub fn relevant_transitioned_from(&self, previous: &Self) -> bool {
        !self.failure_relevant_eq(previous)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompactionCommandKindV1 {
    Manual,
    Automatic,
    AutomaticRetry,
    Overflow,
}

#[derive(Debug)]
pub struct CompactionCommandV1 {
    pub id: u64,
    pub kind: CompactionCommandKindV1,
    pub response: Option<tokio::sync::oneshot::Sender<Result<crate::CompactionOutcome, String>>>,
}

pub fn next_compaction_command_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCompactionDecisionV1 {
    pub failure_class: crate::compactor::CompactionFailureClassV1,
    pub snapshot: CompactionSnapshotV1,
    pub automatic_attempt_consumed: bool,
}

impl PendingCompactionDecisionV1 {
    pub fn wake_once(&mut self, current: &CompactionSnapshotV1) -> bool {
        if self.automatic_attempt_consumed || !current.relevant_transitioned_from(&self.snapshot) {
            return false;
        }
        self.automatic_attempt_consumed = true;
        true
    }
}

/// A cheap marker of "how much the model-visible surface has changed",
/// combining transcript length with a content fingerprint so two
/// same-length-but-different transcripts do not compare as unchanged.
/// Mechanism 7 retries a compaction attempt only when this marker actually
/// moves between attempts -- a backend can report success without changing
/// anything, and without this check that becomes an infinite retry loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceGeneration(u64);

impl SurfaceGeneration {
    pub fn observe(transcript: &Transcript) -> Self {
        let messages = transcript.messages();
        let fp = fingerprint_prefix(messages);
        let hash_component = u64::from_str_radix(&fp[..8.min(fp.len())], 16).unwrap_or(0);
        Self(
            (messages.len() as u64)
                .wrapping_mul(0x9E37_79B1)
                .wrapping_add(hash_component),
        )
    }

    pub fn value(&self) -> u64 {
        self.0
    }
}

/// Mechanism 7. true iff after differs from before -- a retry is only
/// warranted when the surface actually advanced. Equality (including a
/// backend that reported success but changed nothing) blocks the retry.
pub fn retry_is_monotonic(before: SurfaceGeneration, after: SurfaceGeneration) -> bool {
    before != after
}

// ---------------------------------------------------------------------------
// CR-18: per-session pending-retry ledger.
//
// `retry_authoritative` had no production caller: `compact_context` called
// `prepare_authoritative` directly and never retried. This ledger is what
// wires the monotonic helper into the live path WITHOUT turning a failure
// into a hot loop. `SessionEngine::compact_context` consults it before each
// attempt: when a previous attempt failed, the next attempt is routed
// through `Compactor::retry_authoritative`, which refuses to run unless the
// transcript surface actually advanced since the failed attempt -- so a
// retry can only ever happen on a later turn that changed something. Each
// scheduled retry also carries a strictly smaller escalation target (the
// failed attempt's target, halved), and the number of scheduled retries is
// bounded, so a permanently failing compaction eventually gives up instead
// of retrying forever.
//
// Keyed by session id because this fix's file allowlist excludes `engine.rs`
// (same constraint CR-17 solved with the `compaction_coordinator` registry).

/// Upper bound on retries scheduled per failure cycle. After this many
/// consecutive scheduled retries, the ledger gives up and the next attempt
/// runs as a fresh `prepare_authoritative` (which may schedule a new bounded
/// cycle if IT fails).
pub(crate) const MAX_COMPACTION_RETRIES: u32 = 2;

/// One scheduled monotonic retry for a session whose last compaction
/// attempt failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingRetry {
    /// Model-visible surface observed when the failed attempt ran.
    /// `retry_authoritative` refuses to rerun unless the surface has
    /// advanced past this.
    pub generation: SurfaceGeneration,
    /// The strictly-reduced cheap-first escalation target for the retry
    /// (the failed attempt's target, halved).
    pub escalation_trigger_tokens: u32,
    /// How many retries have already been scheduled in this failure cycle.
    pub attempts: u32,
    /// Canonical pending decision. `None` is retained for legacy callers that
    /// record a failure without a snapshot.
    pub decision: Option<PendingCompactionDecisionV1>,
}

fn retry_ledger() -> &'static std::sync::Mutex<std::collections::HashMap<String, PendingRetry>> {
    static LEDGER: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, PendingRetry>>,
    > = std::sync::OnceLock::new();
    LEDGER.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

pub(crate) fn pending_retry_exists(session_id: &str) -> bool {
    retry_ledger()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(session_id)
        .and_then(|pending| pending.decision.as_ref())
        .is_some()
}

pub(crate) fn pending_retry_wake_eligible(
    session_id: &str,
    current: &CompactionSnapshotV1,
) -> bool {
    retry_ledger()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(session_id)
        .and_then(|pending| pending.decision.as_ref())
        .is_some_and(|decision| {
            !decision.automatic_attempt_consumed
                && current.relevant_transitioned_from(&decision.snapshot)
        })
}

/// Admit a pending retry against the current failure-relevant snapshot.
///
/// Admission and consumption of the automatic-attempt bit happen under one
/// ledger lock, so concurrent automatic callers cannot both consume it.
/// Manual callers may inspect/admit a transitioned pending decision, while
/// preserving the automatic bit already recorded for this failure cycle.
pub(crate) fn admit_pending_retry(
    session_id: &str,
    current: &CompactionSnapshotV1,
    kind: CompactionCommandKindV1,
) -> Result<Option<PendingRetry>, String> {
    let mut guard = retry_ledger()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(pending) = guard.get_mut(session_id) else {
        return Ok(None);
    };
    let Some(decision) = pending.decision.as_mut() else {
        return Err(
            "pending compaction retry refused: failure-relevant snapshot unavailable".into(),
        );
    };
    let snapshot_advanced = current.relevant_transitioned_from(&decision.snapshot);
    if !snapshot_advanced {
        return Err("pending compaction retry refused: failure-relevant snapshot unchanged".into());
    }
    if matches!(
        kind,
        CompactionCommandKindV1::Automatic
            | CompactionCommandKindV1::AutomaticRetry
            | CompactionCommandKindV1::Overflow
    ) && !decision.wake_once(current)
    {
        return Err(
            "automatic compaction retry refused: automatic attempt already consumed".into(),
        );
    }
    Ok(Some(pending.clone()))
}

/// Record a failed compaction attempt and schedule at most one bounded
/// sequence of monotonic retries. `target_used` is the escalation target the
/// failed attempt ran with; the scheduled retry gets HALF of it -- strictly
/// smaller, never oscillating. Once [`MAX_COMPACTION_RETRIES`] retries have
/// been scheduled in this cycle, the ledger gives up scheduling but retains
/// the latest pending decision: returns `false` so caller knows no more
/// retries are coming.
pub(crate) fn note_attempt_failed(
    session_id: &str,
    generation: SurfaceGeneration,
    target_used: u32,
    prior_attempts: u32,
    snapshot: Option<CompactionSnapshotV1>,
) -> bool {
    note_attempt_failed_with_class(
        session_id,
        generation,
        target_used,
        prior_attempts,
        snapshot,
        crate::compactor::CompactionFailureClassV1::SummarizerNoProgress,
    )
}

/// Record a failed compaction attempt with its typed failure classification.
/// The automatic-attempt bit remains part of the existing failure cycle.
pub(crate) fn note_attempt_failed_with_class(
    session_id: &str,
    generation: SurfaceGeneration,
    target_used: u32,
    prior_attempts: u32,
    snapshot: Option<CompactionSnapshotV1>,
    failure_class: crate::compactor::CompactionFailureClassV1,
) -> bool {
    let mut guard = retry_ledger()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if prior_attempts >= MAX_COMPACTION_RETRIES {
        // Bounded budget exhausted: give up rather than schedule forever,
        // while retaining canonical decision state until explicit clear.
        // Refresh failure-relevant state so a later admission is evaluated
        // against this latest failed attempt, while preserving consumption.
        if let Some(pending) = guard.get_mut(session_id) {
            pending.generation = generation;
            if let Some(snapshot) = snapshot {
                if let Some(decision) = pending.decision.as_mut() {
                    decision.snapshot = snapshot;
                    decision.failure_class = failure_class;
                } else {
                    pending.decision = Some(PendingCompactionDecisionV1 {
                        failure_class,
                        snapshot,
                        automatic_attempt_consumed: false,
                    });
                }
            }
        }
        return false;
    }
    let automatic_attempt_consumed = guard
        .get(session_id)
        .and_then(|pending| pending.decision.as_ref())
        .is_some_and(|decision| decision.automatic_attempt_consumed);
    guard.insert(
        session_id.to_string(),
        PendingRetry {
            generation,
            escalation_trigger_tokens: target_used / 2,
            attempts: prior_attempts + 1,
            decision: snapshot.map(|snapshot| PendingCompactionDecisionV1 {
                failure_class,
                snapshot,
                automatic_attempt_consumed,
            }),
        },
    );
    true
}

/// Clear any pending retry after a successful attempt (defensive: the entry
/// is normally consumed by `take_pending_retry` before the attempt runs).
pub(crate) fn note_attempt_succeeded(session_id: &str) {
    retry_ledger()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(session_id);
}

/// Drop any pending retry for a session that has ended or is being reset.
pub(crate) fn clear_pending_retry(session_id: &str) {
    note_attempt_succeeded(session_id);
}
