//! F1 durable event identity surface.
//!
//! Three concerns live here:
//!
//! 1. `append_event_at_seq` writes a row at a caller-supplied `seq`, validates
//!    against the cached / persisted high-water mark, and surfaces a typed
//!    `PersistenceGapError` if the requested `seq` would skip or collide with
//!    an already-persisted row. This is the durable-path half of the unified
//!    seq contract — `Conduit::publish` and `append_event_at_seq` share a
//!    single monotonic seq space.
//!
//! 2. `decode_event_row` parses a persisted `EventRow`'s `payload_json` into
//!    a `ProtocolEvent` and canonicalizes identity at the storage boundary.
//!    The event in the storage row is authoritative for its `seq` and
//!    `created_at`; nested `TranscriptMessage` ids are rewritten to the
//!    row-derived canonical form so producer-chosen constant ids (`final`,
//!    `forced-stop`) cannot leak through SSE replay or rehydrate.
//!
//! 3. `EventPersistenceState` is a per-session handle to the latest
//!    published/persisted seq plus a `changed` Notifier the SSE handler /
//!    UI can wait on for visible failure propagation. The actual
//!    engine-side wiring is Commit 4; the contract lives here.

use crate::error::StorageError;
use crate::events::EventRow;
use crate::Storage;
use rightkit_agent_engine_protocol::{
    tool_call_origin_id as proto_tool_call_origin_id, ProtocolEvent,
};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

/// Stable origin-row identifier for a `ToolCallStarted` event at
/// `row_seq`. The awaited engine dispatch consult consults this id when
/// looking up the durable record on replay; a tool result can
/// deterministically find its origin by `(call_id, row_seq)`.
pub fn tool_call_origin_id(call_id: &str, row_seq: i64) -> String {
    proto_tool_call_origin_id(call_id, row_seq)
}

/// A `PersistenceGapError` is returned by `append_event_at_seq` when the
/// requested `seq` would skip over an already-persisted row or collide
/// with one. Surfacing it as a typed error (not a `tracing::warn!`)
/// converts the R2 "fire-and-forget" failure mode into a visible daemon
/// failure that the conductor can retry / propagate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistenceGapError {
    /// The requested seq is greater than `expected_next`. A row was
    /// already persisted at `expected_next` so writing at `requested`
    /// would skip it — there is a hole in the seq space the SSE
    /// replay cannot fill.
    SeqAboveHighWater { requested: i64, high_water: i64 },
    /// The requested seq is less than `existing`. The row at `existing`
    /// is already in the database; writing at `requested` would either
    /// collide (UNIQUE(session_id, seq)) or rewrite history.
    SeqBelowExisting { requested: i64, existing: i64 },
}

impl std::fmt::Display for PersistenceGapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SeqAboveHighWater {
                requested,
                high_water,
            } => write!(
                f,
                "append_event_at_seq would skip high-water: requested={requested}, high_water={high_water}"
            ),
            Self::SeqBelowExisting {
                requested,
                existing,
            } => write!(
                f,
                "append_event_at_seq would collide with existing row: requested={requested}, existing={existing}"
            ),
        }
    }
}

impl std::error::Error for PersistenceGapError {}

/// Errors from `decode_event_row`. The legacy-normalization path never
/// fails — the row is recoverable even when the payload can't be parsed
/// (the canonical event id is `event-<seq>`), but the caller may want to
/// know so the UI can render a tombstone rather than a blank slot.
#[derive(Debug)]
pub enum PersistenceStateError {
    /// The payload claimed a different session than the durable row. The row
    /// is authoritative; callers receive a tombstone instead of exposing an
    /// event from another transcript.
    PayloadSessionMismatch {
        row_session_id: String,
        payload_session_id: String,
    },
    /// The payload's `seq` field disagreed with the row's `seq`. The
    /// row wins; the payload's value is overridden and reported here
    /// for logging.
    PayloadSeqMismatch { row_seq: i64, payload_seq: i64 },
    /// The payload could not be deserialized into `ProtocolEvent`. The
    /// caller may still proceed using a tombstone row, but should log.
    PayloadDecode(String),
}

impl std::fmt::Display for PersistenceStateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PayloadSessionMismatch {
                row_session_id,
                payload_session_id,
            } => write!(
                f,
                "payload session {payload_session_id} disagreed with row session {row_session_id}; row wins"
            ),
            Self::PayloadSeqMismatch {
                row_seq,
                payload_seq,
            } => write!(
                f,
                "payload seq {payload_seq} disagreed with row seq {row_seq}; row wins"
            ),
            Self::PayloadDecode(msg) => write!(f, "payload decode error: {msg}"),
        }
    }
}

impl std::error::Error for PersistenceStateError {}

/// Result of decoding an `EventRow` into a canonical `ProtocolEvent`.
/// `tombstone` is true when the payload could not be parsed; the
/// `event` in that case is a synthetic `SessionStarted` substitute
/// carrying only the row identity so the SSE consumer still gets a
/// stable id to key on.
#[derive(Debug)]
pub struct DecodedEvent {
    pub event: ProtocolEvent,
    /// Canonical normalized identity/correlation for durable facts. Live
    /// projections intentionally have no durable envelope.
    pub envelope: Option<rightkit_agent_engine_protocol::event::EventEnvelopeV1>,
    pub tombstone: bool,
}

/// Per-session persistence cursor. The daemon's persister updates
/// `last_persisted_seq` on every successful `append_event_at_seq`, and
/// updates `last_published_seq` after the corresponding event has been
/// emitted on the Conduit. The `changed` Notifier is what the SSE
/// handler / UI waits on for visible failure propagation — when
/// `last_error` is non-`None` the wait returns and the failure
/// reaches the user.
///
/// This is a *contract* type — the engine-side wiring is Commit 4.
/// Storage owns the type so a future test can construct it without
/// pulling in the daemon crate.
#[derive(Clone)]
pub struct EventPersistenceState {
    inner: Arc<EventPersistenceStateInner>,
}

struct EventPersistenceStateInner {
    snapshot: Mutex<EventPersistenceSnapshot>,
    changed: Condvar,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventPersistenceSnapshot {
    pub last_published_seq: i64,
    pub last_persisted_seq: i64,
    pub last_error: Option<String>,
}

impl EventPersistenceState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(EventPersistenceStateInner {
                snapshot: Mutex::new(EventPersistenceSnapshot::default()),
                changed: Condvar::new(),
            }),
        }
    }

    /// Snapshot of the latest persisted / published seq plus the most
    /// recent error (if any). Cheap; takes the mutex briefly.
    pub fn snapshot(&self) -> EventPersistenceSnapshot {
        self.inner
            .snapshot
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Update the snapshot and notify any waiters. Used by the
    /// engine-side persister (Commit 4) on every successful append and
    /// on every visible failure.
    pub fn update(&self, new_snapshot: EventPersistenceSnapshot) {
        {
            let mut s = self
                .inner
                .snapshot
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            // A late persister task must not move a cursor backwards. Error
            // state is intentionally replaceable (a later successful update
            // clears it), while sequence watermarks are monotonic facts.
            s.last_published_seq = s.last_published_seq.max(new_snapshot.last_published_seq);
            s.last_persisted_seq = s.last_persisted_seq.max(new_snapshot.last_persisted_seq);
            s.last_error = new_snapshot.last_error;
        }
        self.inner.changed.notify_all();
    }

    /// Block until the snapshot changes from `previous`. Returns the
    /// new snapshot. `timeout` is honored; on timeout the current
    /// snapshot is returned unchanged so callers can decide whether to
    /// retry.
    pub fn wait_for_change(
        &self,
        previous: EventPersistenceSnapshot,
        timeout: std::time::Duration,
    ) -> EventPersistenceSnapshot {
        let mut s = self
            .inner
            .snapshot
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let start = Instant::now();
        loop {
            if *s != previous {
                return s.clone();
            }
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return s.clone();
            }
            let (guard, _) = self
                .inner
                .changed
                .wait_timeout(s, timeout - elapsed)
                .unwrap_or_else(|p| p.into_inner());
            s = guard;
        }
    }
}

impl Default for EventPersistenceState {
    fn default() -> Self {
        Self::new()
    }
}

impl Storage {
    /// Append an event at a caller-supplied `seq`. Validates against
    /// the cached / persisted high-water and surfaces a typed
    /// `PersistenceGapError` on gap. Updates the seq cache on
    /// successful insert.
    ///
    /// This is the durable-path half of the unified seq contract: the
    /// conductor must publish the event to the Conduit (with the same
    /// seq) AFTER `append_event_at_seq` returns `Ok` so that an SSE
    /// subscriber receiving the event from the broadcast channel
    /// already has the durable row.
    ///
    /// Like `append_event`, this opens and commits its own `IMMEDIATE`
    /// transaction — never call it from inside another already-open
    /// transaction on this connection, and never from inside
    /// `Storage::commit_compaction` (`compaction_archives.rs`). CRA-11's
    /// atomic archive/projection/lineage/epoch/`sessionCompacted` commit
    /// shares this same unified seq space (it allocates its event rows'
    /// `seq` the same cached-high-water/`MAX(seq)`-fallback way this
    /// function does), but assigns them through its own transaction-local
    /// helper so the whole compaction commits or rolls back as one unit.
    pub fn append_event_at_seq(
        &mut self,
        session_id: &str,
        seq: i64,
        kind: &str,
        payload_json: &str,
    ) -> std::result::Result<(), PersistenceGapError> {
        // A195: raw secrets never reach disk; persisted rows carry irreversible placeholders.
        let redacted_payload =
            rightkit_agent_engine_protocol::secret_scan::redact_json_payload(payload_json);
        let payload_json: &str = &redacted_payload;
        if seq < 1 {
            return Err(PersistenceGapError::SeqAboveHighWater {
                requested: seq,
                high_water: self.seq_cache.get(session_id).copied().unwrap_or(0),
            });
        }
        // Use the cached high-water when we have one (this connection,
        // behind the daemon mutex, is the only writer); otherwise fall
        // back to a one-time MAX(seq) probe.
        let cached_high_water = self.seq_cache.get(session_id).copied();
        let high_water = match cached_high_water {
            Some(c) => c,
            None => {
                let current: i64 = self
                    .conn
                    .query_row(
                        "SELECT COALESCE(MAX(seq), 0) FROM events WHERE session_id = ?1",
                        [session_id],
                        |row| row.get(0),
                    )
                    .map_err(|_| PersistenceGapError::SeqAboveHighWater {
                        requested: seq,
                        high_water: 0,
                    })?;
                // Prime the cache so subsequent appends skip the SELECT.
                self.seq_cache.insert(session_id.to_string(), current);
                current
            }
        };

        if seq > high_water + 1 {
            return Err(PersistenceGapError::SeqAboveHighWater {
                requested: seq,
                high_water,
            });
        }
        if seq <= high_water {
            let identical = self
                .conn
                .query_row(
                    "SELECT kind, payload_json FROM events WHERE session_id=?1 AND seq=?2",
                    rusqlite::params![session_id, seq],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .ok()
                .is_some_and(|(stored_kind, stored_payload)| {
                    let expected = if payload_json.len() > super::append::MAX_EVENT_PAYLOAD_BYTES {
                        super::append::oversized_payload_stub(kind, payload_json)
                    } else {
                        payload_json.to_string()
                    };
                    stored_kind == kind && stored_payload == expected
                });
            if identical {
                return Ok(());
            }
            return Err(PersistenceGapError::SeqBelowExisting {
                requested: seq,
                existing: high_water,
            });
        }

        let tx = rightkit_db::write_txn(&mut self.conn).map_err(|_| {
            PersistenceGapError::SeqAboveHighWater {
                requested: seq,
                high_water,
            }
        })?;
        let created_at = now_iso8601();
        let spilled = super::append::spill_oversized_payload(
            &tx,
            session_id,
            seq,
            kind,
            payload_json,
            &created_at,
        )
        .map_err(|_| PersistenceGapError::SeqAboveHighWater {
            requested: seq,
            high_water,
        })?;
        let stored_payload: &str = spilled.as_deref().unwrap_or(payload_json);
        tx.execute(
            "INSERT INTO events (session_id, seq, kind, payload_json, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (session_id, seq, kind, stored_payload, &created_at),
        )
        .map_err(|_| PersistenceGapError::SeqAboveHighWater {
            requested: seq,
            high_water,
        })?;
        // EVT-005: when a post-message request carries an idempotency key,
        // bind that request's durable admission sequence in the SAME SQLite
        // transaction as UserMessageSubmitted. A crash can therefore expose
        // neither fact, or both facts, but never an ambiguous half-admission.
        if let Ok(ProtocolEvent::UserMessageSubmitted {
            nonce: Some(key), ..
        }) = serde_json::from_str::<ProtocolEvent>(payload_json)
        {
            if !key.trim().is_empty() {
                tx.execute(
                    "UPDATE machine_request_idempotency \
                     SET effect_state = 'admitted', admission_seq = ?4, updated_at = ?5 \
                     WHERE session_id = ?1 AND operation = 'post_message' \
                     AND idempotency_key = ?2 AND effect_state = 'in_progress' \
                     AND response_json IS NULL",
                    (session_id, key.trim(), kind, seq, &created_at),
                )
                .map_err(|_| PersistenceGapError::SeqAboveHighWater {
                    requested: seq,
                    high_water,
                })?;
            }
        }
        tx.commit()
            .map_err(|_| PersistenceGapError::SeqAboveHighWater {
                requested: seq,
                high_water,
            })?;
        self.seq_cache.insert(session_id.to_string(), seq);
        Ok(())
    }

    /// Decode a persisted `EventRow` into a canonical `ProtocolEvent`.
    /// The row's `seq` wins over the payload's `seq` (caller-visible via
    /// `PersistenceStateError::PayloadSeqMismatch`). Nested message ids
    /// are rewritten to the row-derived canonical form. A
    /// non-deserializable payload produces a tombstone event carrying
    /// only the row identity so the SSE consumer can still key on it.
    ///
    /// `on_decode_error` lets the caller log via [`PersistenceStateError`]
    /// before the tombstone is returned; pass a no-op closure to ignore.
    pub fn decode_event_row_with<F>(
        &self,
        row: &EventRow,
        on_decode_error: F,
    ) -> std::result::Result<DecodedEvent, StorageError>
    where
        F: FnOnce(PersistenceStateError),
    {
        let row_seq = row.seq;
        let mut event: ProtocolEvent = match serde_json::from_str(&row.payload_json) {
            Ok(event) => event,
            Err(err) => {
                on_decode_error(PersistenceStateError::PayloadDecode(err.to_string()));
                // Tombstone: the row identity is sufficient to give the
                // SSE consumer a stable key. The synthesized event is
                // `SessionStarted` so the variant matches the
                // `ProtocolEvent` enum without lying about content.
                return Ok(DecodedEvent {
                    event: ProtocolEvent::SessionStarted {
                        session_id: row.session_id.clone(),
                        seq: row_seq,
                        created_at: row.created_at.clone(),
                        title: None,
                    },
                    envelope: None,
                    tombstone: true,
                });
            }
        };
        if event.session_id() != row.session_id {
            on_decode_error(PersistenceStateError::PayloadSessionMismatch {
                row_session_id: row.session_id.clone(),
                payload_session_id: event.session_id().to_string(),
            });
            // A row's session scope is part of its durable identity. Since
            // ProtocolEvent exposes no session-id setter, do not let a valid
            // payload escape under the wrong row/session; retain row identity
            // through the same tombstone path used for malformed payloads.
            return Ok(DecodedEvent {
                event: ProtocolEvent::SessionStarted {
                    session_id: row.session_id.clone(),
                    seq: row_seq,
                    created_at: row.created_at.clone(),
                    title: None,
                },
                envelope: None,
                tombstone: true,
            });
        }
        if event.seq() != row_seq {
            // Caller is responsible for logging via
            // PersistenceStateError::PayloadSeqMismatch; row wins.
            on_decode_error(PersistenceStateError::PayloadSeqMismatch {
                row_seq,
                payload_seq: event.seq(),
            });
        }
        event.canonicalize_identity(row_seq);
        let envelope = if event.is_durable_fact() {
            Some(
                rightkit_agent_engine_protocol::event::EventEnvelopeV1::canonical(event.clone())
                    .map_err(StorageError::Serde)?,
            )
        } else {
            None
        };
        Ok(DecodedEvent {
            event,
            envelope,
            tombstone: false,
        })
    }

    /// Backwards-compatible wrapper that drops the
    /// `PersistenceStateError` callback. The legacy
    /// `decode_event_row` contract was a `tombstone: false` return on
    /// every successful parse; this is preserved. Use
    /// [`Storage::decode_event_row_with`] when the caller wants to
    /// log the missing-id / seq-mismatch signal.
    pub fn decode_event_row(
        &self,
        row: &EventRow,
    ) -> std::result::Result<DecodedEvent, StorageError> {
        self.decode_event_row_with(row, |_| {})
    }
}

/// Best-effort wall-clock timestamp. Matches the format used by
/// `append_event` so the column stays consistent across the two write
/// paths.
fn now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    secs.to_string()
}

/// Compatibility re-export: the F1 contract documents
/// `EventPersistenceState::wait_for_change` and `update` as the
/// surface for visible failure propagation. The notifier-backed
/// `EventPersistenceState::changed` field would require `tokio`; since
/// storage doesn't depend on tokio, callers observe via
/// `wait_for_change` or by polling `snapshot()`.
#[doc(hidden)]
pub use rightkit_db::rusqlite;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{map_event, EventRow};
    use crate::{SessionRow, Storage};
    use rightkit_agent_engine_protocol::TranscriptMessage;

    fn store() -> Storage {
        Storage::open_in_memory().unwrap()
    }

    fn sess(s: &mut Storage, id: &str) {
        s.create_session(&SessionRow {
            id: id.to_string(),
            title: "t".into(),
            workspace_root: "/ws".into(),
            created_at: "t".into(),
            updated_at: "t".into(),
            parent_session_id: None,
            role: None,
            status: "idle".into(),
            config_json: "{}".into(),
            tags: String::new(),
            recovery_marker: None,
        })
        .unwrap();
    }

    fn assistant_event_json(seq: i64, msg_id: &str) -> String {
        let ev = ProtocolEvent::AssistantMessageCompleted {
            session_id: "s".into(),
            seq,
            created_at: "t".into(),
            message: TranscriptMessage::Assistant {
                id: msg_id.into(),
                created_at: "t".into(),
                content: "hi".into(),
                reasoning_content: None,
                tool_calls: vec![],
                pinned: false,
            },
        };
        serde_json::to_string(&ev).unwrap()
    }

    #[test]
    fn append_event_at_seq_writes_at_requested_seq() {
        let mut s = store();
        sess(&mut s, "s");
        let payload = assistant_event_json(1, "final");
        s.append_event_at_seq("s", 1, "assistantMessageCompleted", &payload)
            .unwrap();
        let all = s.all_events("s").unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].seq, 1);
    }

    #[test]
    fn append_event_at_seq_rejects_seq_above_high_water() {
        let mut s = store();
        sess(&mut s, "s");
        s.append_event_at_seq("s", 1, "k", r#"{"seq":1}"#).unwrap();
        // High-water is now 1; requesting 3 must surface a gap error
        // rather than silently skipping.
        let err = s
            .append_event_at_seq("s", 3, "k", r#"{"seq":3}"#)
            .unwrap_err();
        assert!(matches!(
            err,
            PersistenceGapError::SeqAboveHighWater {
                requested: 3,
                high_water: 1
            }
        ));
    }

    #[test]
    fn append_event_at_seq_accepts_exact_idempotent_replay() {
        let mut s = store();
        sess(&mut s, "s");
        s.append_event_at_seq("s", 1, "k", r#"{"seq":1}"#).unwrap();
        s.append_event_at_seq("s", 1, "k", r#"{"seq":1}"#).unwrap();
        assert_eq!(s.all_events("s").unwrap().len(), 1);
    }

    #[test]
    fn append_event_at_seq_rejects_mismatched_seq_below_existing() {
        let mut s = store();
        sess(&mut s, "s");
        s.append_event_at_seq("s", 1, "k", r#"{"seq":1}"#).unwrap();
        s.append_event_at_seq("s", 2, "k", r#"{"seq":2}"#).unwrap();
        let err = s
            .append_event_at_seq("s", 2, "k", r#"{"seq":"different"}"#)
            .unwrap_err();
        assert!(matches!(
            err,
            PersistenceGapError::SeqBelowExisting {
                requested: 2,
                existing: 2
            }
        ));
    }

    #[test]
    fn decode_event_row_canonicalizes_producer_message_id() {
        // Payload carries the producer constant id "final"; row seq is 9.
        let payload = assistant_event_json(9, "final");
        let row = EventRow {
            session_id: "s".into(),
            seq: 9,
            kind: "assistantMessageCompleted".into(),
            payload_json: payload,
            created_at: "t".into(),
        };
        let decoded = store().decode_event_row(&row).unwrap();
        assert!(!decoded.tombstone);
        match &decoded.event {
            ProtocolEvent::AssistantMessageCompleted { message, seq, .. } => {
                assert_eq!(*seq, 9);
                assert_eq!(message.id(), "assistant:9");
            }
            _ => panic!("variant"),
        }
    }

    #[test]
    fn decode_event_row_pins_seq_to_row_even_when_payload_disagrees() {
        // Payload's seq is 99; row seq is 5. Row wins; canonical seq = 5.
        let payload = assistant_event_json(99, "x");
        let row = EventRow {
            session_id: "s".into(),
            seq: 5,
            kind: "assistantMessageCompleted".into(),
            payload_json: payload,
            created_at: "t".into(),
        };
        let decoded = store().decode_event_row(&row).unwrap();
        assert_eq!(decoded.event.seq(), 5);
    }

    #[test]
    fn decode_event_row_tombstones_cross_session_payload() {
        let payload = assistant_event_json(3, "assistant:3");
        let row = EventRow {
            session_id: "authoritative-session".into(),
            seq: 3,
            kind: "assistantMessageCompleted".into(),
            payload_json: payload,
            created_at: "t".into(),
        };
        let mut observed = None;
        let decoded = store()
            .decode_event_row_with(&row, |error| observed = Some(error))
            .unwrap();
        assert!(decoded.tombstone);
        assert_eq!(decoded.event.session_id(), "authoritative-session");
        assert_eq!(decoded.event.seq(), 3);
        assert!(matches!(
            observed,
            Some(PersistenceStateError::PayloadSessionMismatch { .. })
        ));
    }

    #[test]
    fn decode_durable_row_reconstructs_stable_envelope_identity() {
        let storage = Storage::open_in_memory().unwrap();
        let event = ProtocolEvent::SessionTurnFinished {
            session_id: "s".into(),
            seq: 7,
            created_at: "t".into(),
            turn_id: "turn-7".into(),
            status: "completed".into(),
        };
        let row = EventRow {
            session_id: "s".into(),
            seq: 7,
            kind: "sessionTurnFinished".into(),
            payload_json: serde_json::to_string(&event).unwrap(),
            created_at: "t".into(),
        };
        let decoded = storage.decode_event_row(&row).unwrap();
        let envelope = decoded
            .envelope
            .expect("durable row gets canonical envelope");
        assert_eq!(envelope.event_id, "evt-v1:s:7");
        assert_eq!(
            envelope.schema_version,
            rightkit_agent_engine_protocol::event::EVENT_ENVELOPE_SCHEMA_V1
        );
        assert_eq!(envelope.correlation.turn_id.as_deref(), Some("turn-7"));
    }

    #[test]
    fn durable_envelope_identity_survives_database_reopen_without_duplicate_columns() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("event-envelope.sqlite");
        let event = ProtocolEvent::SessionTurnFinished {
            session_id: "s".into(),
            seq: 1,
            created_at: "t".into(),
            turn_id: "turn-1".into(),
            status: "completed".into(),
        };
        let payload = serde_json::to_string(&event).unwrap();
        {
            let mut storage = Storage::open(&db).unwrap();
            storage
                .append_event_at_seq("s", 1, "sessionTurnFinished", &payload)
                .unwrap();
        }

        let storage = Storage::open(&db).unwrap();
        let rows = storage.events_after("s", 0).unwrap();
        assert_eq!(rows.len(), 1);
        let first = storage
            .decode_event_row(&rows[0])
            .unwrap()
            .envelope
            .unwrap();
        let second = storage
            .decode_event_row(&rows[0])
            .unwrap()
            .envelope
            .unwrap();
        assert_eq!(first.event_id, "evt-v1:s:1");
        assert_eq!(first.event_id, second.event_id);
        assert_eq!(first.correlation.turn_id.as_deref(), Some("turn-1"));
    }

    #[test]
    fn decode_projection_row_has_no_durable_envelope() {
        let storage = Storage::open_in_memory().unwrap();
        let event = ProtocolEvent::AssistantDelta {
            session_id: "s".into(),
            seq: 8,
            created_at: "t".into(),
            text: "partial".into(),
            is_reasoning: false,
        };
        let row = EventRow {
            session_id: "s".into(),
            seq: 8,
            kind: "assistantDelta".into(),
            payload_json: serde_json::to_string(&event).unwrap(),
            created_at: "t".into(),
        };
        let decoded = storage.decode_event_row(&row).unwrap();
        assert!(decoded.envelope.is_none());
    }

    #[test]
    fn decode_event_row_returns_tombstone_when_payload_unparseable() {
        let mut s = store();
        sess(&mut s, "s");
        // Insert raw garbage; bypass append_event_at_seq so the row
        // exists independent of payload validity.
        s.conn
            .execute(
                "INSERT INTO events (session_id, seq, kind, payload_json, created_at) \
                 VALUES ('s', 1, 'unknown', 'not-json', 't')",
                [],
            )
            .unwrap();
        let mut stmt = s
            .conn
            .prepare(
                "SELECT session_id, seq, kind, payload_json, created_at \
                 FROM events WHERE session_id = 's'",
            )
            .unwrap();
        let rows: Vec<EventRow> = stmt
            .query_map([], map_event)
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 1);
        let decoded = s.decode_event_row(&rows[0]).unwrap();
        assert!(decoded.tombstone);
        assert_eq!(decoded.event.seq(), 1);
    }

    #[test]
    fn tool_call_origin_id_uses_protocol_helper() {
        // Storage-level helper delegates to the protocol crate so the
        // string shape is identical regardless of where the lookup
        // happens.
        assert_eq!(tool_call_origin_id("tc1", 7), "tool_call_origin:tc1:7");
    }

    #[test]
    fn event_persistence_state_signals_changes() {
        let state = EventPersistenceState::new();
        let initial = state.snapshot();
        assert_eq!(initial.last_persisted_seq, 0);
        assert!(initial.last_error.is_none());

        let mut updated = initial.clone();
        updated.last_persisted_seq = 5;
        state.update(updated.clone());

        // Short wait — the notifier fires immediately, but the wait
        // itself is bounded so a test failure (notify lost) doesn't
        // hang the suite.
        let next = state.wait_for_change(initial, std::time::Duration::from_millis(100));
        assert_eq!(next.last_persisted_seq, 5);
    }

    #[test]
    fn event_persistence_state_does_not_regress_sequence_watermarks() {
        let state = EventPersistenceState::new();
        state.update(EventPersistenceSnapshot {
            last_published_seq: 8,
            last_persisted_seq: 7,
            last_error: None,
        });
        state.update(EventPersistenceSnapshot {
            last_published_seq: 3,
            last_persisted_seq: 2,
            last_error: Some("late failure".into()),
        });
        assert_eq!(
            state.snapshot(),
            EventPersistenceSnapshot {
                last_published_seq: 8,
                last_persisted_seq: 7,
                last_error: Some("late failure".into()),
            }
        );
    }

    #[test]
    fn event_persistence_state_records_error() {
        let state = EventPersistenceState::new();
        let initial = state.snapshot();
        let mut updated = initial.clone();
        updated.last_persisted_seq = 1;
        updated.last_error = Some("disk full".into());
        state.update(updated.clone());

        let next = state.wait_for_change(initial, std::time::Duration::from_millis(100));
        assert_eq!(next.last_persisted_seq, 1);
        assert_eq!(next.last_error.as_deref(), Some("disk full"));
    }
}
