//! The conduit — the single seam between the engine and its subscribers (CLI, UI, remote).
//!
//! Every event is assigned a monotonic `seq` centrally here, buffered in a ring, and fanned
//! out to all live subscribers. A reconnecting client asks for everything after its last-seen
//! seq and the conduit replays from the buffer — this is what makes SSE reconnects gap-free.
//!
//! F1 durable identity:
//! - `Conduit::with_high_water_mark` lets a session resume at a known persisted
//!   seq instead of restarting from 1 — required so an SSE replay after a
//!   daemon restart emits the same seq the client has on disk.
//! - `publish` canonicalizes the event's identity at the boundary: it pins the
//!   seq to whatever the caller already wrote on the event (storage-derived on
//!   the durable path, `next_seq` on the live-only path), and rewrites any
//!   nested `TranscriptMessage` ids to the row-derived canonical form so producer
//!   `final` constants cannot survive a row-replay. The event in the
//!   buffer and the broadcast is the canonicalized event.

use rightkit_agent_engine_protocol::ProtocolEvent;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc};

fn recover_lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub const MAX_HISTORY_EVENTS: usize = 512;
pub const MAX_HISTORY_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_HISTORY_AGE: Duration = Duration::from_secs(15 * 60);

/// A live receive result. Lag is never silently converted into a later event:
/// callers must replay from durable state or terminate their stream.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum ConduitReceive {
    Event(ProtocolEvent),
    Lagged { missed: u64 },
    Closed,
}

/// Replay result for a caller's durable cursor. `ResyncRequired` means no
/// contiguous in-memory replay exists for that cursor.
#[derive(Debug)]
pub enum ConduitReplay {
    Events(Vec<ProtocolEvent>),
    ResyncRequired {
        after_seq: i64,
        oldest_available_seq: i64,
        high_water_mark: i64,
    },
}

#[derive(Clone)]
struct RetainedEvent {
    event: ProtocolEvent,
    bytes: usize,
    retained_at: Instant,
}

/// A subscription to the conduit's live event stream.
pub struct ConduitSubscription {
    rx: broadcast::Receiver<ProtocolEvent>,
    /// Present only for a human client ([`Conduit::subscribe_human`]); counts it as attached
    /// until the subscription is dropped.
    _human: Option<HumanClientGuard>,
}

/// Keeps one human client counted in [`Conduit::human_subscriber_count`] while it lives.
struct HumanClientGuard(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl Drop for HumanClientGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl ConduitSubscription {
    /// Non-blocking receive of an already-published live event.
    pub fn try_recv(&mut self) -> Option<ProtocolEvent> {
        self.rx.try_recv().ok()
    }

    /// Await one live result without hiding dropped events.
    pub async fn recv(&mut self) -> ConduitReceive {
        match self.rx.recv().await {
            Ok(event) => ConduitReceive::Event(event),
            Err(broadcast::error::RecvError::Lagged(missed)) => ConduitReceive::Lagged { missed },
            Err(broadcast::error::RecvError::Closed) => ConduitReceive::Closed,
        }
    }
}

/// The event conduit. Assigns seq numbers, buffers recent history, fans out live.
pub struct Conduit {
    tx: broadcast::Sender<ProtocolEvent>,
    /// Ring buffer of recent events for catch-up replay.
    history: Mutex<VecDeque<RetainedEvent>>,
    history_cap: usize,
    history_byte_cap: usize,
    next_seq: Mutex<i64>,
    persistence_gate: Mutex<Option<mpsc::UnboundedSender<ProtocolEvent>>>,
    /// Latched, first-reason-wins persistence failure. In-memory only: it is
    /// never itself written as a durable event (that would recurse into the
    /// failed persistence path).
    persistence_failure: Mutex<Option<String>>,
    /// Carries the first latch reason (set exactly once) when the failure latches, so owners of
    /// in-flight work can cancel it without polling.
    latch_signal: tokio::sync::watch::Sender<Option<String>>,
    /// Live subscriptions opened for a human client (tools rule 1: only these can answer an
    /// approval or a question). Headless streams, orchestrators and tests use plain
    /// [`Conduit::subscribe`] and are not counted.
    human_subscribers: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

/// A durable fact was refused because session persistence has failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("session persistence failed: {reason}")]
pub struct PersistenceLatched {
    pub reason: String,
}

impl Conduit {
    pub fn new(buffer: usize) -> Self {
        Self::with_high_water_mark(buffer, 0)
    }

    /// Construct a conduit whose `next_seq` cursor starts one past
    /// `high_water`. Use this when resuming a session so the first event
    /// published after restart has a seq strictly greater than the last
    /// seq the storage layer persisted — this is what makes the
    /// `seq_on_event == seq_on_row` invariant hold across restarts.
    ///
    /// A `high_water` of 0 is equivalent to `Conduit::new` — the first
    /// event published will be seq 1.
    pub fn with_high_water_mark(buffer: usize, high_water: i64) -> Self {
        let history_cap = buffer.clamp(1, MAX_HISTORY_EVENTS);
        let (tx, _rx) = broadcast::channel(history_cap);
        let start = if high_water < 0 {
            1
        } else {
            // Saturate to 1 for the new-session case; resume adds 1 above
            // the last persisted seq.
            high_water.saturating_add(1).max(1)
        };
        Self {
            tx,
            history: Mutex::new(VecDeque::with_capacity(history_cap)),
            history_cap,
            history_byte_cap: MAX_HISTORY_BYTES,
            next_seq: Mutex::new(start),
            persistence_gate: Mutex::new(None),
            persistence_failure: Mutex::new(None),
            latch_signal: tokio::sync::watch::channel(None).0,
            human_subscribers: Default::default(),
        }
    }

    /// Latch a persistence failure for this session. Returns true only for the
    /// first call; later reasons are ignored so the root cause is preserved.
    pub fn latch_persistence_failure(&self, reason: impl Into<String>) -> bool {
        let mut slot = recover_lock(&self.persistence_failure);
        if slot.is_some() {
            return false;
        }
        let reason = reason.into();
        tracing::error!(%reason, "session persistence latched as failed");
        *slot = Some(reason.clone());
        drop(slot);
        self.latch_signal.send_replace(Some(reason));
        true
    }

    /// Receiver that observes `Some(reason)` once persistence latches as failed. It
    /// does not keep the conduit alive: `wait_for`/`changed` error when the
    /// conduit is dropped.
    pub fn subscribe_latch(&self) -> tokio::sync::watch::Receiver<Option<String>> {
        self.latch_signal.subscribe()
    }

    /// The first persistence failure reason, if the session has latched one.
    /// Live-only health read; needs no durable storage.
    pub fn persistence_failure(&self) -> Option<String> {
        recover_lock(&self.persistence_failure).clone()
    }

    /// Highest seq currently allocated by this conduit. Use it after
    /// resume to decide whether a session still has a live in-memory
    /// tail above what the storage layer has on disk.
    pub fn current_high_water(&self) -> i64 {
        let s = *recover_lock(&self.next_seq);
        s.saturating_sub(1)
    }

    /// Adopt rows committed by a transactional sink before allocating another
    /// live fact. Persistence still validates every admitted event sequence.
    pub fn advance_committed_high_water(&self, high_water: i64) {
        let mut next = recover_lock(&self.next_seq);
        *next = (*next).max(high_water.saturating_add(1));
    }

    /// Subscribe to future live events.
    pub fn subscribe(&self) -> ConduitSubscription {
        ConduitSubscription {
            rx: self.tx.subscribe(),
            _human: None,
        }
    }

    /// Subscribe as a human client (desktop, web, TUI, remote card surface): while the returned
    /// subscription lives the session counts as attended, so an approval or question waits for
    /// an answer instead of being denied as `unattended` (tools rule 1). A headless `run` stream
    /// must use [`subscribe`](Self::subscribe).
    pub fn subscribe_human(&self) -> ConduitSubscription {
        self.human_subscribers
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        ConduitSubscription {
            rx: self.tx.subscribe(),
            _human: Some(HumanClientGuard(self.human_subscribers.clone())),
        }
    }

    /// Human clients currently attached ([`subscribe_human`](Self::subscribe_human)).
    pub fn human_subscriber_count(&self) -> usize {
        self.human_subscribers
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Allocate and return the next seq value.
    pub fn next_seq(&self) -> i64 {
        let mut s = recover_lock(&self.next_seq);
        let v = *s;
        *s += 1;
        v
    }

    /// Install the persistence admission queue. Every sequenced event enters
    /// this queue before persistence. Live projections are still broadcast
    /// immediately; durable facts are withheld until [`Conduit::publish_committed`].
    /// This explicit ingress avoids making durability depend on subscribing to
    /// the same broadcast that durable facts are intentionally withheld from.
    pub fn install_persistence_gate(&self) -> mpsc::UnboundedReceiver<ProtocolEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        *recover_lock(&self.persistence_gate) = Some(tx);
        rx
    }

    /// Broadcast a durable fact only after its authoritative SQLite append
    /// committed. This path deliberately bypasses the persistence ingress so
    /// the committed fact cannot be re-enqueued and persisted twice.
    pub fn publish_committed(&self, event: ProtocolEvent) {
        self.publish_live(event);
    }

    /// Publish an event. The caller's `seq` field on the event payload is
    /// authoritative — on the durable path it came from the storage row, on
    /// the live-only path it came from `next_seq()`. The event is
    /// canonicalized at this boundary so any nested `TranscriptMessage` ids
    /// are rewritten to row-derived canonical form. Live projections are then
    /// fanned out directly; durable facts are fanned out by `publish_committed`
    /// only after their SQLite append succeeds.
    ///
    /// Legacy infallible form: a refused durable fact is dropped here, but the
    /// latch (see [`Self::persistence_failure`]) is already set and the engine
    /// loop stops on it. Producers that can act on refusal use [`Self::try_publish`].
    pub fn publish(&self, event: ProtocolEvent) {
        let _ = self.try_publish(event);
    }

    /// Fallible publish. A durable fact returns `Err` (without allocating a
    /// seq or attempting a send) once persistence has latched as failed, and
    /// latches + returns `Err` if the persistence queue has no receiver.
    /// Live projections are unaffected by the latch.
    pub fn try_publish(&self, mut event: ProtocolEvent) -> Result<(), PersistenceLatched> {
        // EVT-001/EVT-002: live projections never take a durable seq, never
        // enter the persistence queue and are not retained for replay. They
        // are anchored to the newest allocated durable seq so ordering
        // consumers can merge them with committed facts.
        if !event.is_durable_fact() {
            let anchor = self.current_high_water();
            event.canonicalize_identity(anchor);
            let _ = self.tx.send(event);
            return Ok(());
        }
        if let Some(reason) = self.persistence_failure() {
            return Err(PersistenceLatched { reason });
        }
        // Live-only callers use seq=0 and expect the conduit to allocate the
        // next sequence. Durable replay callers provide a positive storage
        // sequence, which must remain authoritative across restarts.
        let supplied_seq = seq_of(&event);
        let canonical_seq = if supplied_seq > 0 {
            // Durable rows are authoritative, but they still advance the
            // allocator. Otherwise a later live event could reuse a sequence
            // already published from storage and break monotonic replay.
            let mut next = recover_lock(&self.next_seq);
            *next = (*next).max(supplied_seq.saturating_add(1));
            supplied_seq
        } else {
            self.next_seq()
        };
        event.canonicalize_identity(canonical_seq);
        if let Some(gate) = recover_lock(&self.persistence_gate).as_ref() {
            // Only durable facts reach here (live projections returned above).
            // Fail closed if the persistence worker has disappeared: a
            // durable fact must never become visible without its commit.
            if gate.send(event).is_err() {
                let reason = "persistence worker stopped; durable fact not stored".to_string();
                self.latch_persistence_failure(reason.clone());
                return Err(PersistenceLatched {
                    reason: self.persistence_failure().unwrap_or(reason),
                });
            }
            return Ok(());
        }
        self.publish_live(event);
        Ok(())
    }

    /// Build and publish an event whose seq is allocated atomically with
    /// construction. A durable fact consumes the next durable seq; a live
    /// projection consumes none (the allocator stays gap-free for storage).
    pub fn emit(&self, make: impl FnOnce(i64) -> ProtocolEvent) {
        let event = {
            let mut next = recover_lock(&self.next_seq);
            let seq = *next;
            let event = make(seq);
            if event.is_durable_fact() {
                *next = seq.saturating_add(1);
            }
            event
        };
        self.publish(event);
    }

    fn publish_live(&self, event: ProtocolEvent) {
        {
            let mut h = recover_lock(&self.history);
            prune_expired(&mut h);
            let bytes = encoded_len(&event);
            while !h.is_empty()
                && (h.len() >= self.history_cap
                    || retained_bytes(&h).saturating_add(bytes) > self.history_byte_cap)
            {
                h.pop_front();
            }
            // A single oversize event is still delivered live, but is not
            // retained because it would defeat the byte bound.
            if bytes <= self.history_byte_cap {
                h.push_back(RetainedEvent {
                    event: event.clone(),
                    bytes,
                    retained_at: Instant::now(),
                });
            }
        }
        let _ = self.tx.send(event);
    }

    /// All buffered events with seq strictly greater than `after_seq`, in order.
    pub fn events_after(&self, after_seq: i64) -> Vec<ProtocolEvent> {
        match self.replay_after(after_seq) {
            ConduitReplay::Events(events) => events,
            ConduitReplay::ResyncRequired { .. } => Vec::new(),
        }
    }

    /// Return a contiguous replay or make resynchronization explicit.
    pub fn replay_after(&self, after_seq: i64) -> ConduitReplay {
        let mut history = recover_lock(&self.history);
        prune_expired(&mut history);
        // Durable replay may publish storage-assigned sequences without using
        // this process's allocation counter, so account for both sources.
        let high_water_mark = self.current_high_water().max(
            history
                .back()
                .map(|retained| seq_of(&retained.event))
                .unwrap_or(0),
        );
        let Some(first) = history.front() else {
            return if after_seq < high_water_mark {
                ConduitReplay::ResyncRequired {
                    after_seq,
                    oldest_available_seq: high_water_mark.saturating_add(1),
                    high_water_mark,
                }
            } else {
                ConduitReplay::Events(Vec::new())
            };
        };
        let oldest_available_seq = seq_of(&first.event);
        if after_seq.saturating_add(1) < oldest_available_seq {
            return ConduitReplay::ResyncRequired {
                after_seq,
                oldest_available_seq,
                high_water_mark,
            };
        }
        ConduitReplay::Events(
            history
                .iter()
                .filter(|retained| seq_of(&retained.event) > after_seq)
                .map(|retained| retained.event.clone())
                .collect(),
        )
    }

    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }

    pub fn history_len(&self) -> usize {
        let mut history = recover_lock(&self.history);
        prune_expired(&mut history);
        history.len()
    }
}

fn encoded_len(event: &ProtocolEvent) -> usize {
    serde_json::to_vec(event)
        .map(|encoded| encoded.len())
        .unwrap_or(0)
}

fn retained_bytes(history: &VecDeque<RetainedEvent>) -> usize {
    history.iter().map(|retained| retained.bytes).sum()
}

fn prune_expired(history: &mut VecDeque<RetainedEvent>) {
    while history
        .front()
        .is_some_and(|retained| retained.retained_at.elapsed() > MAX_HISTORY_AGE)
    {
        history.pop_front();
    }
}

/// Extract the seq from any `ProtocolEvent` variant.
pub fn seq_of(e: &ProtocolEvent) -> i64 {
    use ProtocolEvent::*;
    match e {
        SessionStarted { seq, .. }
        | SessionTitleUpdated { seq, .. }
        | AssistantDelta { seq, .. }
        | AssistantMessageCompleted { seq, .. }
        | ToolCallStarted { seq, .. }
        | ToolApprovalRequested { seq, .. }
        | ToolApprovalResolved { seq, .. }
        | ToolPolicyEvent { seq, .. }
        | ToolResultAdded { seq, .. }
        | SessionTurnFinished { seq, .. }
        | StreamResyncRequired { seq, .. }
        | Overloaded { seq, .. }
        | PersistenceFailed { seq, .. }
        | ProviderRetrying { seq, .. }
        | SessionCompactionStarted { seq, .. }
        | VerificationFinished { seq, .. }
        | TaskGraphUpdated { seq, .. }
        | TaskStatusUpdated { seq, .. }
        | TaskArtifactUpdated { seq, .. }
        | ArtifactLineageUpdated { seq, .. }
        | RunInterrupted { seq, .. }
        | RunFailed { seq, .. }
        | SessionCompacted { seq, .. }
        | SessionContextFolded { seq, .. }
        | UsageUpdated { seq, .. }
        | TurnUsage { seq, .. }
        | PromptCacheBreak { seq, .. }
        | ToolOutputDelta { seq, .. }
        | BackgroundShellExited { seq, .. }
        | BudgetExceeded { seq, .. }
        | ProviderRouteUpdated { seq, .. }
        | AccountRateLimited { seq, .. }
        | AccountUsageReported { seq, .. }
        | DispatchDecided { seq, .. }
        | OrchestrationObservationRecorded { seq, .. }
        | AgentQueryIssued { seq, .. }
        | AgentQueryResponded { seq, .. }
        | HumanSteeringInjected { seq, .. }
        | SubAgentSpawned { seq, .. }
        | SubAgentCompleted { seq, .. }
        | SubAgentCancelled { seq, .. }
        | UserMessageSubmitted { seq, .. }
        | UserQuestionAsked { seq, .. }
        | McpElicitationRequested { seq, .. }
        | CredentialRequested { seq, .. }
        | CredentialResolved { seq, .. }
        | UserQuestionAnswered { seq, .. }
        | PlanPresented { seq, .. }
        | AgentRosterUpdated { seq, .. }
        | CheckRunCompleted { seq, .. }
        | GoalProgressUpdated { seq, .. }
        | SelfReviewProposalReceived { seq, .. }
        | ModelRouteDecision { seq, .. }
        | ToolApprovalReviewed { seq, .. }
        | RunGovernanceRejected { seq, .. } => *seq,
    }
}
