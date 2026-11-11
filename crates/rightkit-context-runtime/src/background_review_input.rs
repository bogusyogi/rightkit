//! CTX-023/CTX-024 background-review **input producer**.
//!
//! The resident's background scheduler consumes a
//! [`BackgroundReviewInputSnapshotV1`] from a workspace file. Before this
//! module existed nothing in the product ever wrote that file, so the
//! deterministic semantic provider and the CTX-024 foreground-coverage gate
//! were live code over data that never arrived.
//!
//! This module is the missing producer. It runs **inside the resident serve
//! process**, on a real turn boundary (a completed Adapt observation window),
//! and it publishes only state Cortex already holds:
//!
//! * the append-only session-event stream that
//!   `adapt_observations::execute` wrote for this scope/session/task, and
//! * whatever durable Cortex knowledge already covers that seq range.
//!
//! It introduces no process of its own, it cannot run while the resident is not serving
//! (there is no serve path to call it), and it writes nothing when
//! there is no new recorded activity: an empty window stays an honest no-op.

use crate::MemoryStore;
use rightkit_context_protocol::background_review::{
    BackgroundReviewActivitySignalV1, BackgroundReviewEventRangeV1,
    BackgroundReviewForegroundMemoryStateV1, BackgroundReviewJobKindV1,
};
use rightkit_db::rusqlite;
use rightkit_memory_store::{AbsorbedStore, MemDb, SessionEvent};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Env override honoured by the daemon reader; mirrored here so producer and
/// consumer can never resolve different files.
pub const BACKGROUND_REVIEW_INPUT_ENV: &str = "RIGHTKIT_CONTEXT_BACKGROUND_REVIEW_INPUT";
pub const DEFAULT_BACKGROUND_REVIEW_INPUT: &str = ".membrane/background-review-input.json";
pub const SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// Detector identity used by `adapt_observations` to name the per-consumer
/// event stream. Duplicated deliberately (the constant is private to that
/// module) and pinned by
/// `derived_stream_matches_the_stream_adapt_observations_writes`, so drift
/// fails a test instead of silently producing snapshots for an empty stream.
const ADAPT_VERIFICATION_DETECTOR: &str = "required_verification_completion.v1";

/// Upper bound on one published window. Matches the daemon loader's own
/// `cursor.last_seq + 256` bound, so a published window is never larger than
/// the window the consumer will read.
const MAX_WINDOW_EVENTS: u64 = 256;

/// How long a published window stays reviewable. The daemon treats this as a
/// hard deadline for one review attempt.
const REVIEW_DEADLINE_MS: u64 = 60_000;

/// Host activity/foreground snapshot the daemon scheduler consumes. The field
/// set and wire names are exactly the daemon's reader contract
/// (`deny_unknown_fields`, camelCase, `taskId` optional).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackgroundReviewInputSnapshotV1 {
    pub schema_version: u32,
    pub activity: BackgroundReviewActivitySignalV1,
    pub job_kind: BackgroundReviewJobKindV1,
    pub foreground_memory_state: BackgroundReviewForegroundMemoryStateV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub deadline_unix_ms: u64,
}

/// What one successful publication announced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedWindow {
    pub path: PathBuf,
    pub session_id: String,
    pub from_seq: u64,
    pub to_seq: u64,
}

/// Resolve the snapshot path. Installed state (`.../Membrane/state`) is
/// authoritative and ignores the env override; every other root honours it.
/// The daemon calls this same function, so the two cannot drift.
pub fn input_path(root: &Path) -> PathBuf {
    if root.file_name().is_some_and(|name| name == "state")
        && root
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "Membrane")
    {
        return root.join(DEFAULT_BACKGROUND_REVIEW_INPUT);
    }
    std::env::var_os(BACKGROUND_REVIEW_INPUT_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join(DEFAULT_BACKGROUND_REVIEW_INPUT))
}

/// Durable producer cursor: which seq of each stream has already been
/// announced. Kept beside the snapshot so a crash cannot lose it.
fn cursor_path(root: &Path) -> PathBuf {
    let snapshot = input_path(root);
    let mut name = snapshot
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "background-review-input.json".to_string());
    name.push_str(".cursor");
    snapshot.with_file_name(name)
}

/// Event stream `adapt_observations::execute` appends a completed window to.
pub fn adapt_consumer_stream(scope: &str, session_id: &str, task_id: &str) -> String {
    format!(
        "adapt:consumer:{}",
        rightkit_adapt::canonical::sha256_canonical(&json!([
            scope,
            session_id,
            task_id,
            ADAPT_VERIFICATION_DETECTOR
        ]))
    )
}

/// Temp file + fsync + rename, so a reader never observes a partial snapshot.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "snapshot path has no parent directory".to_string())?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    rightkit_fs::atomic_write(path, bytes).map_err(|error| error.to_string())
}
fn read_published_cursors(root: &Path) -> std::collections::BTreeMap<String, u64> {
    std::fs::read_to_string(cursor_path(root))
        .ok()
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or_default()
}

/// Publish the pending review window for one completed observation window.
///
/// Returns `Ok(None)` when there is genuinely nothing to report (no recorded
/// stream, or no event past the last announced seq) — the honest no-op. It
/// returns `Err` only when real recorded activity exists but could not be
/// published, so the caller can log a refusal instead of silently degrading.
#[allow(clippy::too_many_arguments)]
pub fn publish_observation_window(
    store: &MemoryStore,
    root: &Path,
    scope: &str,
    session_id: &str,
    task_id: &str,
    window_id: &str,
    foreground_active: bool,
    observed_at_unix_ms: u64,
) -> Result<Option<PublishedWindow>, String> {
    let stream = adapt_consumer_stream(scope, session_id, task_id);
    publish_stream_window(
        store,
        root,
        &stream,
        task_id,
        window_id,
        foreground_active,
        observed_at_unix_ms,
    )
}

#[allow(clippy::too_many_arguments)]
fn publish_stream_window(
    store: &MemoryStore,
    root: &Path,
    stream: &str,
    task_id: &str,
    window_id: &str,
    foreground_active: bool,
    observed_at_unix_ms: u64,
) -> Result<Option<PublishedWindow>, String> {
    // One publisher at a time: the producer cursor is read-modify-written, and
    // two turn boundaries completing together must not lose an advance.
    static PUBLISH: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serialized = PUBLISH.lock().unwrap_or_else(|error| error.into_inner());
    let events = AbsorbedStore::new(store.db().clone()).map_err(|error| error.to_string())?;
    let high_water = events.cursor(stream).map_err(|error| error.to_string())?;
    if high_water.last_seq == 0 {
        // No recorded activity for this stream: never fabricate a window.
        return Ok(None);
    }
    let published = read_published_cursors(root);
    let last_published = published.get(stream).copied().unwrap_or(0);
    if high_water.last_seq <= last_published {
        // Replayed or idempotent window; nothing new was appended.
        return Ok(None);
    }
    let from_seq = last_published.saturating_add(1);
    let to_seq = high_water
        .last_seq
        .min(last_published.saturating_add(MAX_WINDOW_EVENTS));
    let window = events
        .events_range(stream, from_seq, to_seq.saturating_add(1))
        .map_err(|error| error.to_string())?;
    if window.is_empty() {
        return Ok(None);
    }
    // Binding check: the stream we derived must be the stream the window we
    // were told about actually landed in. If the detector identity ever drifts
    // this refuses loudly rather than publishing a snapshot over a stream that
    // holds someone else's events.
    let holds_window = window
        .iter()
        .any(|event| event.payload.get("window_id").and_then(Value::as_str) == Some(window_id));
    if !holds_window {
        return Err(format!(
            "derived observation stream does not hold window {window_id}"
        ));
    }

    let activity_units = window.len() as u64;
    let input_tokens: u64 = window
        .iter()
        .map(|event| rightkit_memory_core::estimate_tokens(&event.payload.to_string()) as u64)
        .sum::<u64>()
        .max(1);
    let observed_at = window
        .iter()
        .map(|event| event.occurred_at_ms)
        .max()
        .filter(|value| *value != 0)
        .unwrap_or(observed_at_unix_ms);

    let snapshot = BackgroundReviewInputSnapshotV1 {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        activity: BackgroundReviewActivitySignalV1 {
            schema_version: BackgroundReviewActivitySignalV1::SCHEMA_VERSION,
            session_id: stream.to_string(),
            turn_id: window_id.to_string(),
            activity_units,
            input_tokens: Some(input_tokens),
            foreground_active,
            observed_at_unix_ms: observed_at,
        },
        job_kind: BackgroundReviewJobKindV1::AdaptBehavioralReview,
        foreground_memory_state: foreground_memory_state(
            &store.db().clone(),
            stream,
            from_seq,
            to_seq.saturating_add(1),
        ),
        task_id: (!task_id.trim().is_empty()).then(|| task_id.to_string()),
        deadline_unix_ms: observed_at.saturating_add(REVIEW_DEADLINE_MS),
    };
    snapshot
        .activity
        .validate()
        .map_err(|error| error.to_string())?;
    snapshot
        .foreground_memory_state
        .validate()
        .map_err(|error| error.to_string())?;

    let path = input_path(root);
    let encoded = serde_json::to_vec(&snapshot).map_err(|error| error.to_string())?;
    write_atomic(&path, &encoded)?;

    // The producer cursor advances only after the snapshot is durably in
    // place. A crash in between republishes the identical window on the next
    // boundary (level-triggered, idempotent); the daemon's own cursor decides
    // what it has already consumed, so no window is lost or double-consumed.
    let mut published = published;
    published.insert(stream.to_string(), to_seq);
    let encoded_cursor = serde_json::to_vec(&published).map_err(|error| error.to_string())?;
    write_atomic(&cursor_path(root), &encoded_cursor)?;

    Ok(Some(PublishedWindow {
        path,
        session_id: stream.to_string(),
        from_seq,
        to_seq,
    }))
}

/// Durable knowledge Cortex already holds for a session's seq range.
///
/// This reports **what Cortex knows**, never a decision: the CTX-024 gate
/// re-derives its own window and re-checks overlap independently, so the two
/// cannot disagree about admission. Coverage means a durable knowledge
/// emission for this session that was **not** produced by a background review
/// job (background emissions carry `jobId`); those are foreground memory.
///
/// * proposal registry unreadable → `Unavailable` (the gate then fails closed);
/// * registry present/never initialised and nothing covers the range →
///   `AvailableNoEmission`;
/// * a covering foreground emission → `AvailableEmission { range }`.
pub fn foreground_memory_state(
    db: &MemDb,
    session_id: &str,
    from_seq: u64,
    to_seq: u64,
) -> BackgroundReviewForegroundMemoryStateV1 {
    match foreground_coverage(db, session_id, from_seq, to_seq) {
        Ok(Some(range)) => BackgroundReviewForegroundMemoryStateV1::AvailableEmission { range },
        Ok(None) => BackgroundReviewForegroundMemoryStateV1::AvailableNoEmission,
        Err(_) => BackgroundReviewForegroundMemoryStateV1::Unavailable,
    }
}

fn foreground_coverage(
    db: &MemDb,
    session_id: &str,
    from_seq: u64,
    to_seq: u64,
) -> Result<Option<BackgroundReviewEventRangeV1>, String> {
    for emission in session_emissions(db, session_id)? {
        // Background-produced candidates carry the originating jobId; they are
        // not foreground memory and must not suppress the background lane.
        if emission.get("jobId").is_some_and(|value| !value.is_null()) {
            continue;
        }
        let (Some(start), Some(end)) = (
            emission.get("fromSeq").and_then(Value::as_u64),
            emission.get("toSeq").and_then(Value::as_u64),
        ) else {
            continue;
        };
        let range = BackgroundReviewEventRangeV1 {
            start_seq: start,
            end_seq: end,
        };
        if range.validate().is_err() {
            continue;
        }
        if range.overlaps(BackgroundReviewEventRangeV1 {
            start_seq: from_seq,
            end_seq: to_seq,
        }) {
            return Ok(Some(range));
        }
    }
    Ok(None)
}

/// Highest seq of this session that durable Cortex knowledge already covers,
/// counting every producer. Used to seed a restarted daemon's review cursor so
/// a crash does not replay windows that already produced durable knowledge.
pub fn durable_reviewed_through_seq(db: &MemDb, session_id: &str) -> Option<u64> {
    session_emissions(db, session_id)
        .ok()?
        .into_iter()
        .filter_map(|emission| emission.get("toSeq").and_then(Value::as_u64))
        .filter(|value| *value > 0)
        .max()
        .map(|to_seq| to_seq.saturating_sub(1))
}

/// Knowledge emissions recorded for one session. `Err` means the registry
/// could not be consulted; a missing table means it was never initialised, so
/// no emission exists and the honest answer is an empty list.
fn session_emissions(db: &MemDb, session_id: &str) -> Result<Vec<Value>, String> {
    let conn = db.lock_events();
    let mut statement = match conn.prepare(
        "SELECT emission_json FROM membrane_knowledge_proposal
         WHERE emission_json LIKE ?1 ORDER BY created_at ASC LIMIT 512",
    ) {
        Ok(statement) => statement,
        Err(rusqlite::Error::SqliteFailure(_, Some(message)))
            if message.contains("no such table") =>
        {
            return Ok(Vec::new());
        }
        Err(rusqlite::Error::SqlInputError { ref msg, .. }) if msg.contains("no such table") => {
            return Ok(Vec::new());
        }
        Err(error) => return Err(error.to_string()),
    };
    let pattern = format!("%{session_id}%");
    let rows = statement
        .query_map([pattern], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?;
    let mut emissions = Vec::new();
    for row in rows {
        let text = row.map_err(|error| error.to_string())?;
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if value.get("sessionId").and_then(Value::as_str) == Some(session_id) {
            emissions.push(value);
        }
    }
    Ok(emissions)
}

/// Test/consumer helper: the events one published window refers to.
pub fn window_events(
    store: &MemoryStore,
    session_id: &str,
    from_seq: u64,
    to_seq: u64,
) -> Result<Vec<SessionEvent>, String> {
    AbsorbedStore::new(store.db().clone())
        .map_err(|error| error.to_string())?
        .events_range(session_id, from_seq, to_seq.saturating_add(1))
        .map_err(|error| error.to_string())
}
