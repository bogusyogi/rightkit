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
    /// immediately; durable facts are withheld until [`publish_committed`].
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
        debug_assert!(event.is_durable_fact());
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
        | ToolPolicyEvent { seq, .. }
        | RunGovernanceRejected { seq, .. } => *seq,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: i64) -> ProtocolEvent {
        durable_ev(seq)
    }

    fn projection_ev(seq: i64) -> ProtocolEvent {
        ProtocolEvent::AssistantDelta {
            session_id: "s1".into(),
            seq,
            created_at: "t".into(),
            text: format!("e{seq}"),
            is_reasoning: false,
        }
    }

    fn durable_ev(seq: i64) -> ProtocolEvent {
        ProtocolEvent::UserMessageSubmitted {
            session_id: "s1".into(),
            seq,
            created_at: "t".into(),
            text: format!("u{seq}"),
            nonce: None,
            trust: None,
            attachments: Vec::new(),
        }
    }

    #[test]
    fn seq_is_monotonic() {
        let c = Conduit::new(16);
        assert_eq!(c.next_seq(), 1);
        assert_eq!(c.next_seq(), 2);
        assert_eq!(c.next_seq(), 3);
    }

    #[test]
    fn seq_of_reads_agent_roster_seq() {
        let ev = ProtocolEvent::AgentRosterUpdated {
            session_id: "s".into(),
            seq: 42,
            created_at: "t".into(),
            agents: vec![],
        };
        assert_eq!(seq_of(&ev), 42);
    }

    #[test]
    fn events_after_filters() {
        let c = Conduit::new(16);
        for i in 1..=5 {
            c.publish(ev(i));
        }
        let got = c.events_after(2);
        assert_eq!(got.len(), 3);
        assert_eq!(seq_of(&got[0]), 3);
        assert_eq!(seq_of(&got[2]), 5);
    }

    #[test]
    fn events_after_zero_returns_all_buffered() {
        let c = Conduit::new(16);
        c.publish(ev(1));
        c.publish(ev(2));
        assert_eq!(c.events_after(0).len(), 2);
    }

    #[test]
    fn ring_buffer_evicts_oldest() {
        let c = Conduit::new(3);
        for i in 1..=5 {
            c.publish(ev(i));
        }
        assert_eq!(c.history_len(), 3);
        // Only the last 3 remain (seq 3,4,5)
        let all = c.events_after(2);
        assert_eq!(seq_of(&all[0]), 3);
        assert_eq!(seq_of(&all[2]), 5);
    }

    #[tokio::test]
    async fn live_subscriber_receives() {
        let c = Conduit::new(16);
        let mut sub = c.subscribe();
        c.publish(ev(1));
        let ConduitReceive::Event(got) = sub.recv().await else {
            panic!("expected live event");
        };
        assert_eq!(seq_of(&got), 1);
    }

    #[tokio::test]
    async fn persistence_gate_withholds_durable_until_committed_but_not_projection() {
        let c = Conduit::new(16);
        let mut live = c.subscribe();
        let mut persistence = c.install_persistence_gate();

        c.publish(durable_ev(1));
        let queued = persistence.recv().await.expect("durable admission");
        assert_eq!(seq_of(&queued), 1);
        assert!(queued.is_durable_fact());
        assert!(tokio::time::timeout(Duration::from_millis(20), live.recv())
            .await
            .is_err());

        c.publish_committed(queued);
        let ConduitReceive::Event(committed) = live.recv().await else {
            panic!("expected committed durable fact");
        };
        assert_eq!(seq_of(&committed), 1);

        // EVT-002: a live projection never enters the persistence queue,
        // takes no durable seq, and is anchored to the newest allocated seq.
        c.publish(projection_ev(0));
        assert!(persistence.try_recv().is_err());
        let ConduitReceive::Event(projected) = live.recv().await else {
            panic!("expected live projection");
        };
        assert!(!projected.is_durable_fact());
        assert_eq!(seq_of(&projected), 1);
        assert_eq!(c.next_seq(), 2, "projection must not consume a durable seq");
    }

    #[tokio::test]
    async fn missing_persistence_worker_never_exposes_durable_fact() {
        let c = Conduit::new(16);
        let mut live = c.subscribe();
        let persistence = c.install_persistence_gate();
        drop(persistence);

        c.publish(durable_ev(1));
        assert!(tokio::time::timeout(Duration::from_millis(20), live.recv())
            .await
            .is_err());
        assert_eq!(c.history_len(), 0);
    }

    #[tokio::test]
    async fn dropped_gate_receiver_latches_failure_and_publish_errors() {
        let c = Conduit::new(16);
        let persistence = c.install_persistence_gate();
        drop(persistence);

        assert!(c.persistence_failure().is_none());
        let err = c.try_publish(durable_ev(1)).expect_err("send must fail");
        assert!(!err.reason.is_empty());
        assert_eq!(c.persistence_failure(), Some(err.reason));
        assert_eq!(c.history_len(), 0);
    }

    #[tokio::test]
    async fn latched_conduit_fails_fast_without_sending_or_allocating_seq() {
        let c = Conduit::new(16);
        let mut persistence = c.install_persistence_gate();
        assert!(c.latch_persistence_failure("disk full"));

        let err = c.try_publish(durable_ev(0)).expect_err("latched");
        assert_eq!(err.reason, "disk full");
        assert!(persistence.try_recv().is_err(), "nothing may be enqueued");
        assert_eq!(c.next_seq(), 1, "refused fact must not consume a seq");
    }

    #[tokio::test]
    async fn tool_output_delta_never_reaches_persistence_gate() {
        let c = Conduit::new(16);
        let mut live = c.subscribe();
        let mut persistence = c.install_persistence_gate();
        c.publish(ProtocolEvent::ToolOutputDelta {
            session_id: "s1".into(),
            seq: 0,
            created_at: "t".into(),
            call_id: "c1".into(),
            stream: "stdout".into(),
            text: "out".into(),
        });
        assert!(
            persistence.try_recv().is_err(),
            "toolOutputDelta must never be enqueued for storage"
        );
        let ConduitReceive::Event(delta) = live.recv().await else {
            panic!("expected live toolOutputDelta");
        };
        assert!(!delta.is_durable_fact());
        assert_eq!(c.next_seq(), 1, "live delta must not consume a durable seq");
    }

    #[tokio::test]
    async fn live_deltas_still_deliver_after_latch() {
        let c = Conduit::new(16);
        let mut live = c.subscribe();
        let _persistence = c.install_persistence_gate();
        c.latch_persistence_failure("disk full");

        assert!(c.try_publish(projection_ev(0)).is_ok());
        let ConduitReceive::Event(delta) = live.recv().await else {
            panic!("expected live delta");
        };
        assert!(!delta.is_durable_fact());
    }

    #[test]
    fn latch_records_first_reason_only() {
        let c = Conduit::new(16);
        assert!(c.latch_persistence_failure("first"));
        assert!(!c.latch_persistence_failure("second"));
        assert_eq!(c.persistence_failure().as_deref(), Some("first"));
    }

    #[tokio::test]
    async fn multi_subscriber_both_receive() {
        let c = Conduit::new(16);
        let mut a = c.subscribe();
        let mut b = c.subscribe();
        c.publish(ev(7));
        let ConduitReceive::Event(a_event) = a.recv().await else {
            panic!("expected first subscriber event");
        };
        let ConduitReceive::Event(b_event) = b.recv().await else {
            panic!("expected second subscriber event");
        };
        assert_eq!(seq_of(&a_event), 7);
        assert_eq!(seq_of(&b_event), 7);
    }

    #[test]
    fn high_water_mark_resume_starts_first_seq_above_persisted() {
        let c = Conduit::with_high_water_mark(16, 17);
        // First allocation is one above the last persisted seq.
        assert_eq!(c.next_seq(), 18);
        assert_eq!(c.next_seq(), 19);
        assert_eq!(c.current_high_water(), 19);
    }

    #[test]
    fn high_water_mark_zero_is_equivalent_to_new() {
        let c = Conduit::with_high_water_mark(16, 0);
        assert_eq!(c.next_seq(), 1);
        assert_eq!(c.current_high_water(), 1);
    }

    #[test]
    fn high_water_mark_saturates_negative_to_one() {
        let c = Conduit::with_high_water_mark(16, -5);
        assert_eq!(c.next_seq(), 1);
    }

    #[test]
    fn publish_canonicalizes_nested_message_id_at_boundary() {
        use rightkit_agent_engine_protocol::TranscriptMessage;
        let c = Conduit::new(16);
        let event = ProtocolEvent::AssistantMessageCompleted {
            session_id: "s".into(),
            seq: 9,
            created_at: "t".into(),
            message: TranscriptMessage::Assistant {
                id: "producer-final".into(),
                created_at: "t".into(),
                content: "hi".into(),
                reasoning_content: None,
                tool_calls: vec![],
                pinned: false,
            },
        };
        c.publish(event);
        let buffered = c.events_after(8);
        assert_eq!(buffered.len(), 1);
        match &buffered[0] {
            ProtocolEvent::AssistantMessageCompleted { seq, message, .. } => {
                assert_eq!(*seq, 9);
                assert_eq!(message.id(), "assistant:9");
            }
            _ => panic!("expected AssistantMessageCompleted"),
        }
    }

    #[test]
    fn zero_seq_live_event_gets_a_conduit_sequence() {
        let c = Conduit::with_high_water_mark(16, 41);
        c.publish(ev(0));
        let buffered = c.events_after(41);
        assert_eq!(buffered.len(), 1);
        assert_eq!(seq_of(&buffered[0]), 42);
        assert_eq!(c.current_high_water(), 42);
    }

    #[test]
    fn publish_pins_seq_to_event_payload_not_conduit_counter() {
        // Durable path: storage handed us a row with seq=42; we publish
        // with seq=42 even though next_seq() would have produced 1.
        let c = Conduit::new(16);
        let _ = c.next_seq(); // advance conduit counter to 1
        c.publish(ev(42));
        let buffered = c.events_after(41);
        assert_eq!(seq_of(&buffered[0]), 42);
        // Durable publication advances the live allocator so a subsequent
        // live event cannot reuse the persisted sequence.
        assert_eq!(c.next_seq(), 43);
    }

    #[test]
    fn replay_requires_resync_when_ring_evicted_requested_sequence() {
        let c = Conduit::new(3);
        for seq in 1..=5 {
            c.publish(ev(seq));
        }
        assert!(matches!(
            c.replay_after(1),
            ConduitReplay::ResyncRequired {
                after_seq: 1,
                oldest_available_seq: 3,
                high_water_mark: 5,
            }
        ));
    }

    #[tokio::test]
    async fn live_lag_is_reported_not_silently_skipped() {
        let c = Conduit::new(1);
        let mut sub = c.subscribe();
        c.publish(ev(1));
        c.publish(ev(2));
        assert!(matches!(
            sub.recv().await,
            ConduitReceive::Lagged { missed } if missed >= 1
        ));
    }

    #[test]
    fn replay_requires_resync_when_ring_empty_and_cursor_behind_high_water() {
        // A rehydrated session resumes its conduit above the persisted
        // high-water mark with an empty ring. A cursor at the persisted
        // seq cannot be replayed contiguously from memory alone.
        let c = Conduit::with_high_water_mark(16, 10);
        assert!(matches!(
            c.replay_after(4),
            ConduitReplay::ResyncRequired {
                after_seq: 4,
                oldest_available_seq: 11,
                high_water_mark: 10,
            }
        ));
    }

    #[test]
    fn replay_is_empty_not_resync_when_cursor_at_high_water_and_ring_empty() {
        let c = Conduit::with_high_water_mark(16, 10);
        assert!(matches!(c.replay_after(10), ConduitReplay::Events(events) if events.is_empty()));
    }

    #[test]
    fn poisoned_sequence_lock_recovers_without_cascading_panic() {
        let c = Conduit::new(4);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = c.next_seq.lock().unwrap();
            panic!("inject sequence-lock poison");
        }));
        assert_eq!(c.next_seq(), 1);
        c.publish(ev(0));
        assert_eq!(c.current_high_water(), 2);
    }

    #[test]
    fn poisoned_history_lock_recovers_and_replay_remains_available() {
        let c = Conduit::new(4);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = c.history.lock().unwrap();
            panic!("inject history-lock poison");
        }));
        c.publish(ev(1));
        assert_eq!(c.history_len(), 1);
        assert!(matches!(c.replay_after(0), ConduitReplay::Events(events) if events.len() == 1));
    }
}
