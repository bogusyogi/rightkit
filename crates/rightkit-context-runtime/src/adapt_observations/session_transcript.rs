//! Canonical CodeRight session-transcript ingestion into the absorbed store.
//!
//! The producer is CodeRight's native runtime exporter; it appends committed
//! durable `ProtocolEvent` projections — never live-only rows — as
//! `coderight_event_v1` normalized rows. Identity, ordering, scope, and
//! provenance are verified structurally before any write, and the append-only
//! store provides the sequencing, replay, and tombstone primitives:
//!
//! * exact replay returns `AlreadyPresent` and counts toward
//!   `already_present` without writing;
//! * a reused `event_id` over different content is an event conflict;
//! * a skipped or out-of-order `seq` is a cursor mismatch;
//! * a session that ran `session_transcript_forget` retains its event
//!   tombstones and is permanently gone — later appends answer `410`.
//!
//! Rows are stored verbatim so `membrane-transcript`'s `coderight` adapter
//! normalizes them into the existing Adapt input shape; this module adds no
//! second learning path.

use rightkit_context_protocol::session_transcript::{
    SessionTranscriptAppendReceiptV1, SessionTranscriptAppendV1, SessionTranscriptCursorTipV1,
    SessionTranscriptEventV1, SESSION_TRANSCRIPT_EVENT_TYPE, SESSION_TRANSCRIPT_HOST,
    SESSION_TRANSCRIPT_SCHEMA_VERSION,
};
use rightkit_db::rusqlite::{self, OptionalExtension};
use rightkit_memory_store::{
    AbsorbedStoreError, AppendOutcome, ProvenanceRef, SessionEvent, ABSORBED_SCHEMA_VERSION,
};
use serde_json::{json, Value};

use crate::MemoryStore;

/// Typed append failure so HTTP can map it to a status the producer can act
/// on without parsing prose. `Invalid` covers malformed/invalid requests
/// (400); conflict and tombstone states carry their resync details.
#[derive(Debug)]
pub enum SessionTranscriptError {
    Invalid(String),
    CursorMismatch(SessionTranscriptCursorTipV1),
    EventConflict { stream_id: String, event_id: String },
    Gone { session_id: String, seq: u64 },
}

impl SessionTranscriptError {
    pub fn status(&self) -> u16 {
        match self {
            Self::Invalid(_) => 400,
            Self::CursorMismatch(_) | Self::EventConflict { .. } => 409,
            Self::Gone { .. } => 410,
        }
    }

    /// The error body carries the typed detail fields at top level so the
    /// native transport's whole-body `details` map exposes them directly.
    pub fn body(&self) -> String {
        match self {
            Self::Invalid(detail) => {
                json!({"error":"adapt_observation_refused","detail":detail}).to_string()
            }
            Self::CursorMismatch(tip) => json!({
                "code": "transcript_cursor_mismatch",
                "error": "transcript cursor mismatch",
                "streamId": tip.stream_id,
                "lastSeq": tip.last_seq,
                "lastSourceSeq": tip.last_source_seq,
                "prefixDigest": tip.prefix_digest,
            })
            .to_string(),
            Self::EventConflict {
                stream_id,
                event_id,
            } => json!({
                "code": "transcript_event_conflict",
                "error": "transcript event conflict",
                "streamId": stream_id,
                "eventId": event_id,
            })
            .to_string(),
            Self::Gone { session_id, seq } => json!({
                "code": "transcript_session_gone",
                "error": "session transcript is forgotten",
                "sessionId": session_id,
                "seq": seq,
            })
            .to_string(),
        }
    }

    /// String form for the non-HTTP `execute` callers, preserving the typed
    /// code so status collapses never lose the machine-readable reason.
    pub fn detail(&self) -> String {
        match self {
            Self::Invalid(detail) => detail.clone(),
            Self::CursorMismatch(tip) => format!(
                "transcript_cursor_mismatch stream={} lastSeq={} lastSourceSeq={}",
                tip.stream_id, tip.last_seq, tip.last_source_seq
            ),
            Self::EventConflict { event_id, .. } => {
                format!("transcript_event_conflict {event_id}")
            }
            Self::Gone { session_id, seq } => {
                format!("transcript_session_gone {session_id}:{seq}")
            }
        }
    }
}

fn store_error(error: impl std::fmt::Display) -> SessionTranscriptError {
    SessionTranscriptError::Invalid(format!("transcript store unavailable: {error}"))
}

fn ledger(
    store: &MemoryStore,
) -> Result<rightkit_memory_store::AbsorbedStore, SessionTranscriptError> {
    rightkit_memory_store::AbsorbedStore::new(store.db().clone()).map_err(store_error)
}

/// A session is permanently gone once `session_transcript_forget` ran: all
/// its event rows were deleted while its position tombstones remain (plus
/// the `seq = 0` stream sentinel `forget` writes). A stream that was never
/// deleted either has no tombstones at all, or still holds live rows beside
/// per-event tombstones.
pub(crate) fn session_forgotten(
    store: &MemoryStore,
    session_id: &str,
) -> Result<bool, SessionTranscriptError> {
    let conn = store.db().lock();
    let has_tombstones: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM absorbed_event_tombstones WHERE session_id=?1)",
            [session_id],
            |row| row.get(0),
        )
        .map_err(store_error)?;
    let has_rows: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM absorbed_events WHERE session_id=?1)",
            [session_id],
            |row| row.get(0),
        )
        .map_err(store_error)?;
    Ok(has_tombstones && !has_rows)
}

/// Committed-stream tip at the store's current cursor.
pub(crate) fn tip(
    db: &rightkit_memory_store::AbsorbedStore,
    session_id: &str,
) -> Result<SessionTranscriptCursorTipV1, SessionTranscriptError> {
    let cursor = db.cursor(session_id).map_err(store_error)?;
    tip_at(db, session_id, cursor.last_seq)
}

/// Committed-stream tip bound to a specific stream position: the tip event's
/// source position and content hash, bound into `prefixDigest`. Callers that
/// must prove a recorded prefix (e.g. Taste stream-source reverification)
/// use this rather than the live cursor so a stream that has grown since
/// does not move the comparison.
pub(crate) fn tip_at(
    db: &rightkit_memory_store::AbsorbedStore,
    session_id: &str,
    tip_seq: u64,
) -> Result<SessionTranscriptCursorTipV1, SessionTranscriptError> {
    let tip_event = if tip_seq > 0 {
        db.events_range(session_id, tip_seq, tip_seq + 1)
            .map_err(store_error)?
            .into_iter()
            .next()
    } else {
        None
    };
    let (last_source_seq, event_id, content_hash) = match tip_event.as_ref() {
        Some(event) => (
            event
                .payload
                .get("sourceSeq")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            event.event_id.as_str(),
            event.content_hash.as_str(),
        ),
        None => (0, "", ""),
    };
    let prefix_digest = format!(
        "sha256:{}",
        rightkit_adapt_taste::canonical::sha256_canonical(&json!([
            tip_seq,
            event_id,
            content_hash
        ]))
    );
    Ok(SessionTranscriptCursorTipV1 {
        stream_id: session_id.to_string(),
        last_seq: tip_seq,
        last_source_seq,
        prefix_digest,
    })
}

/// The committed end of a session's stream: the position the next append must extend. A
/// forgotten session answers `Gone`, as an append does.
pub fn cursor_tip(
    store: &MemoryStore,
    session_id: &str,
) -> Result<SessionTranscriptCursorTipV1, SessionTranscriptError> {
    let db = ledger(store)?;
    if session_forgotten(store, session_id)? {
        return Err(SessionTranscriptError::Gone {
            session_id: session_id.to_string(),
            seq: 0,
        });
    }
    tip(&db, session_id)
}

/// The `session_transcript_tip` answer: `{sessionTranscriptTip}` with the stream's committed end,
/// or the typed refusal. Body: `{"session_id": "<id>"}`.
pub fn tip_response(store: &MemoryStore, body: &str) -> (u16, String) {
    let session_id = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .filter(|id| !id.trim().is_empty());
    let Some(session_id) = session_id else {
        return (
            400,
            json!({"error":"adapt_observation_refused","detail":"session_id is required"})
                .to_string(),
        );
    };
    match cursor_tip(store, &session_id) {
        Ok(tip) => (200, json!({"sessionTranscriptTip": tip}).to_string()),
        Err(error) => (error.status(), error.body()),
    }
}

/// Structural row binding: every normalized row must belong to the declared
/// session, carry its own `sourceSeq`/`eventId`, and name CodeRight plus the
/// durable source in provenance. A row that disagrees with its envelope is
/// malformed, not merely mismatched.
fn validate_row(
    session_id: &str,
    event: &SessionTranscriptEventV1,
) -> Result<(), SessionTranscriptError> {
    let invalid = |detail: &str| SessionTranscriptError::Invalid(detail.to_string());
    if event.event_type != SESSION_TRANSCRIPT_EVENT_TYPE {
        return Err(invalid("unsupported transcript row contract"));
    }
    if event.event_id != format!("evt-v1:{session_id}:{}", event.source_seq) {
        return Err(invalid(
            "event identity does not bind session and source sequence",
        ));
    }
    let payload = &event.payload;
    if payload.get("type").and_then(Value::as_str) != Some(SESSION_TRANSCRIPT_EVENT_TYPE) {
        return Err(invalid("row type must be coderight_event_v1"));
    }
    if payload.get("host").and_then(Value::as_str) != Some(SESSION_TRANSCRIPT_HOST) {
        return Err(invalid("row host must be coderight"));
    }
    if payload.get("sessionId").and_then(Value::as_str) != Some(session_id) {
        return Err(invalid("row session does not match the stream"));
    }
    if payload.get("sourceSeq").and_then(Value::as_u64) != Some(event.source_seq) {
        return Err(invalid("row sourceSeq does not match the event envelope"));
    }
    if payload.get("eventId").and_then(Value::as_str) != Some(event.event_id.as_str()) {
        return Err(invalid("row eventId does not match the event envelope"));
    }
    let provenance = payload.get("provenance").and_then(Value::as_object);
    let Some(provenance) = provenance else {
        return Err(invalid("row provenance is required"));
    };
    if provenance.get("source").and_then(Value::as_str) != Some("coderight") {
        return Err(invalid("row provenance source must be coderight"));
    }
    if provenance.get("producer").and_then(Value::as_str).is_none() {
        return Err(invalid("row provenance producer is required"));
    }
    Ok(())
}

/// The committed row's `recorded_at_ms` for `event_id`, when one exists.
/// `payload_json` stores the whole `SessionEvent`, so a retried append with a
/// fresh timestamp would fail the store's whole-record identity check as an
/// `EventIdConflict`; reusing the committed timestamp keeps honest replays
/// `AlreadyPresent` while any semantic difference still conflicts.
fn prior_recorded_ms(
    store: &MemoryStore,
    event_id: &str,
) -> Result<Option<u64>, SessionTranscriptError> {
    let conn = store.db().lock();
    conn.query_row(
        "SELECT recorded_at_ms FROM absorbed_events WHERE event_id=?1",
        [event_id],
        |row| row.get::<_, i64>(0),
    )
    .optional()
    .map(|value| value.map(|ms| ms.max(0) as u64))
    .map_err(store_error)
}

/// Append one bounded batch into the session's absorbed stream.
pub fn append(
    store: &MemoryStore,
    request: SessionTranscriptAppendV1,
) -> Result<SessionTranscriptAppendReceiptV1, SessionTranscriptError> {
    request
        .validate()
        .map_err(|error| SessionTranscriptError::Invalid(error.to_string()))?;
    let db = ledger(store)?;
    if session_forgotten(store, &request.session_id)? {
        return Err(SessionTranscriptError::Gone {
            session_id: request.session_id,
            seq: 0,
        });
    }
    for event in &request.events {
        validate_row(&request.session_id, event)?;
    }
    let cursor = db.cursor(&request.session_id).map_err(store_error)?;
    // A producer ahead of our tip declares events we never saw; reject before
    // partially committing so a mismatch cannot leave a gap behind a written
    // prefix.
    if request.expected_stream_seq > cursor.last_seq {
        return Err(SessionTranscriptError::CursorMismatch(tip(
            &db,
            &request.session_id,
        )?));
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let mut inserted = 0u64;
    let mut already_present = 0u64;
    for event in &request.events {
        // Byte-stable replay: `payload_json` stores the whole record, so a
        // retried append must reuse the committed `recorded_at_ms` for the
        // store's whole-record identity check to answer `AlreadyPresent`
        // instead of a false conflict on a fresh timestamp.
        let recorded_at_ms = prior_recorded_ms(store, &event.event_id)?.unwrap_or(now_ms);
        let record = SessionEvent {
            schema_version: ABSORBED_SCHEMA_VERSION,
            session_id: request.session_id.clone(),
            seq: event.seq,
            event_id: event.event_id.clone(),
            event_type: event.event_type.clone(),
            payload: event.payload.clone(),
            scope_id: request.scope_id.clone(),
            authority: "A0".into(),
            influence_class: "reference".into(),
            lifecycle: "active".into(),
            retention: "local_audit".into(),
            provenance: vec![ProvenanceRef {
                source: SESSION_TRANSCRIPT_HOST.into(),
                source_event_ids: vec![event.event_id.clone()],
                producer: event
                    .payload
                    .get("provenance")
                    .and_then(|provenance| provenance.get("producer"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
            }],
            content_hash: rightkit_adapt_taste::canonical::sha256_canonical(&event.payload),
            occurred_at_ms: event.occurred_at_unix_ms,
            recorded_at_ms,
        };
        match db.append_event(&record) {
            Ok(AppendOutcome::Inserted(_)) => inserted += 1,
            Ok(AppendOutcome::AlreadyPresent(_)) => already_present += 1,
            Err(AbsorbedStoreError::Tombstoned { session_id, seq }) => {
                return Err(SessionTranscriptError::Gone { session_id, seq });
            }
            Err(AbsorbedStoreError::EventIdConflict(event_id)) => {
                return Err(SessionTranscriptError::EventConflict {
                    stream_id: request.session_id.clone(),
                    event_id,
                });
            }
            Err(
                AbsorbedStoreError::SequenceGap { .. } | AbsorbedStoreError::DuplicateSequence(_),
            ) => {
                return Err(SessionTranscriptError::CursorMismatch(tip(
                    &db,
                    &request.session_id,
                )?));
            }
            Err(error) => return Err(SessionTranscriptError::Invalid(error.to_string())),
        }
    }
    let tip = tip(&db, &request.session_id)?;
    Ok(SessionTranscriptAppendReceiptV1 {
        schema_version: SESSION_TRANSCRIPT_SCHEMA_VERSION,
        stream_id: request.session_id,
        inserted,
        already_present,
        last_seq: tip.last_seq,
        last_source_seq: tip.last_source_seq,
        prefix_digest: tip.prefix_digest,
    })
}

/// Forget the session's transcript stream: delete its rows, retain the
/// position tombstones, and make later appends answer `410`. Idempotent.
///
/// `delete_session` only tombstones rows that existed; a session forgotten
/// before its first export has none. The `seq = 0` sentinel below is below
/// every valid stream position, so `append`'s per-seq tombstone probe never
/// matches it — it exists solely so `session_forgotten` (and any later
/// append's guard) sees a permanently-forgotten stream instead of an empty
/// one that could resurrect.
pub fn forget(store: &MemoryStore, session_id: &str) -> Result<Value, SessionTranscriptError> {
    if session_id.trim().is_empty() || session_id.len() > 512 {
        return Err(SessionTranscriptError::Invalid(
            "invalid session identity".into(),
        ));
    }
    let db = ledger(store)?;
    // `delete_session`'s return only covers the absorbed_sessions row; a
    // transcript-only stream (the common case) has committed events but no
    // session record, so report "had" over either. Probe live rows rather
    // than the cursor — the cursor row survives deletion and would keep a
    // repeat forget reporting content that is already gone.
    let had_events = !db
        .events_range(session_id, 1, 2)
        .map_err(store_error)?
        .is_empty();
    let existed = db.delete_session(session_id).map_err(store_error)? | had_events;
    {
        let conn = store.db().lock();
        conn.execute(
            "INSERT OR IGNORE INTO absorbed_event_tombstones\
             (session_id,seq,event_id,tombstoned_at_ms) VALUES (?1,0,?2,?3)",
            rusqlite::params![
                session_id,
                "session-transcript-forget",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_millis() as i64)
                    .unwrap_or(0),
            ],
        )
        .map_err(store_error)?;
    }
    Ok(json!({
        "sessionId": session_id,
        "forgotten": true,
        "hadSession": existed,
    }))
}
