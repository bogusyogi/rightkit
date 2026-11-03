//! Native RightKit session events -> Membrane context ingestion, over Pulse's
//! `membrane` CLI (`transcript append`, `adapt observe`) via the adapter crate.
//!
//! This exporter reads the **durable** RightKit event log (already gated
//! through `spawn_event_persister`), projects each durable `ProtocolEvent` into
//! the `coderight_event_v1` normalized row and appends bounded batches with a
//! cursor-checked `transcript append`.
//!
//! Ownership lines it must not cross:
//!
//! * RightKit owns execution and the durable session event log; this module
//!   only reads committed rows, never writes session truth.
//! * Pulse's Membrane owns stream sequencing; a cursor conflict returns its
//!   committed end, which this exporter adopts.
//! * Membrane owns normalization and Adapt learning downstream of the
//!   absorbed store; the exporter ships rows verbatim and adds no second
//!   learning path.
//!
//! Durable-cursor discipline: `membrane_transcript_cursors` records the
//! `(last_stream_seq, last_source_seq)` pair per session and advances only
//! through compare-and-set, so a restarted daemon and a retried trigger land
//! idempotent replays rather than divergent writes. A session-gone conflict
//! forgets the cursor row itself. Pulse absent: every export is a no-op.
//!
//! No CLI equivalent yet (Orthic-Labs/pulse#129): transcript forget and
//! `mine_session`; both return an explicit "not available" error or are not
//! attempted.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use rightkit_agent_engine_protocol::{ProtocolEvent, TranscriptMessage};
use rightkit_agent_engine_storage::{
    AdaptObservationCursor, EventPersistenceState, TranscriptExportCursor,
};
use rightkit_memory::membrane::{MembraneBackend, MembraneCli, TranscriptAppend};
use serde_json::{json, Value};

use crate::api::{lock_recover, SharedStorage};

/// Shared Membrane backend for `DaemonState::with_membrane`: the verified Pulse
/// CLI, else `Unavailable`. Discovery goes through `MembraneHandle::shared()`, the
/// same handle the memory backend uses.
pub(crate) fn membrane_backend() -> Arc<dyn MembraneBackend + Send + Sync> {
    rightkit_memory::backend::MembraneHandle::shared().backend_arc()
}

/// Rows per `transcript append` call.
const MAX_EVENTS_PER_APPEND: usize = 256;
/// `coderight_event_v1` row identity.
const SESSION_TRANSCRIPT_EVENT_TYPE: &str = "coderight_event_v1";
const SESSION_TRANSCRIPT_HOST: &str = "coderight";
/// Explicit error for operations Pulse's CLI does not expose yet.
const NOT_AVAILABLE_VIA_CLI: &str = "not available via Membrane CLI (Orthic-Labs/pulse#129)";

/// Provenance marker carried on every normalized row.
const EXPORTER_PRODUCER: &str = "coderight-daemon";

/// How one export pass ended. Transient failures stay typed so callers can
/// log them; only terminal stream states change durable behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportOutcome {
    /// The durable tail up to the last read is committed in the worker.
    UpToDate { exported: u64 },
    /// The worker reports this session's stream forgotten; the local cursor
    /// was deleted and no further export is attempted.
    Gone,
    /// The session rows were deleted (or the session id was tombstoned)
    /// before export could run.
    SessionRemoved,
    /// A committable batch or the local cursor write failed; a later trigger
    /// resumes from the durable cursor.
    Failed(String),
}

/// Coordinates session-scoped export against the shared worker transport.
/// One exporter instance lives on `DaemonState`; per-session serialization
/// guards keep concurrent triggers (turn end, session end, deletion) from
/// racing the same stream.
pub struct NativeTranscriptExporter {
    /// `None` when Pulse is not installed: exports are no-ops.
    cli: Option<MembraneCli>,
    session_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl NativeTranscriptExporter {
    /// Locate Pulse's `membrane` binary; absent leaves the exporter inert.
    pub fn new() -> Self {
        Self::with_cli(rightkit_memory::backend::MembraneHandle::shared().verified_cli())
    }

    pub fn with_cli(cli: Option<MembraneCli>) -> Self {
        Self {
            cli,
            session_locks: Mutex::new(HashMap::new()),
        }
    }

    /// Stream seq to source seq: the source seq of the `stream_seq`-th durable
    /// fact of the session (0 when `stream_seq` is 0).
    fn source_seq_for_stream(
        storage: &SharedStorage,
        session_id: &str,
        stream_seq: u64,
    ) -> Result<i64, String> {
        if stream_seq == 0 {
            return Ok(0);
        }
        let store = storage.blocking_lock();
        let rows = store
            .events_after(session_id, 0)
            .map_err(|error| format!("replay durable events: {error}"))?;
        let mut count = 0u64;
        for row in rows {
            let Ok(decoded) = store.decode_event_row(&row) else {
                continue;
            };
            if decoded.tombstone || !decoded.event.is_durable_fact() {
                continue;
            }
            count += 1;
            if count == stream_seq {
                return Ok(row.seq);
            }
        }
        Err(format!(
            "membrane stream end {stream_seq} is beyond the local durable log ({count} facts)"
        ))
    }

    /// Project content-free H4 facts from durable protocol rows. A separate
    /// durable cursor and worker window sequence make retries/restarts safe.
    fn export_execution_observations(
        &self,
        storage: &SharedStorage,
        session_id: &str,
        scope_id: &str,
    ) {
        let Some(cli) = self.cli.as_ref() else {
            return;
        };
        let (mut cursor, rows) = {
            let store = storage.blocking_lock();
            let task_id = format!("session:{session_id}");
            let cursor = match store.adapt_observation_cursor(session_id, &task_id) {
                Ok(cursor) => cursor.unwrap_or_default(),
                Err(error) => {
                    tracing::warn!(session_id = %session_id, %error, "could not read H4 observation cursor");
                    return;
                }
            };
            let rows = match store.events_after(session_id, cursor.last_source_seq) {
                Ok(rows) => rows,
                Err(error) => {
                    tracing::warn!(session_id = %session_id, %error, "could not replay durable events for H4");
                    return;
                }
            };
            (cursor, rows)
        };
        if rows.is_empty() {
            return;
        }
        let initial_cursor = cursor.clone();
        // No `ProviderRouteUpdated` fact is emitted on the normal path, so fall back
        // to the route the session was configured with (`provider/model`).
        let configured_route = {
            let store = storage.blocking_lock();
            store
                .get_session(session_id)
                .ok()
                .flatten()
                .and_then(|row| serde_json::from_str::<Value>(&row.config_json).ok())
                .and_then(|cfg| cfg.get("model").and_then(Value::as_str).map(str::to_owned))
                .and_then(|full| {
                    let (provider, model) = full.split_once('/')?;
                    (!provider.is_empty() && !model.is_empty())
                        .then(|| (provider.to_owned(), model.to_owned()))
                })
        };
        let mut route: Option<(String, String)> = initial_cursor
            .provider
            .clone()
            .zip(initial_cursor.model.clone())
            .or(configured_route);
        let mut observations = Vec::new();
        let mut last_source_seq = cursor.last_source_seq;
        let mut observation_source_tail = cursor.last_source_seq;
        let task_id = format!("session:{session_id}");
        for row in rows.into_iter().take(4096) {
            last_source_seq = row.seq;
            let store = storage.blocking_lock();
            let event = match store.decode_event_row(&row) {
                Ok(decoded) if !decoded.tombstone => decoded.event,
                _ => continue,
            };
            drop(store);
            if let ProtocolEvent::ProviderRouteUpdated {
                provider, model, ..
            } = &event
            {
                if !provider.trim().is_empty() && !model.trim().is_empty() {
                    route = Some((provider.clone(), model.clone()));
                    cursor.provider = Some(provider.clone());
                    cursor.model = Some(model.clone());
                }
            }
            if matches!(&event, ProtocolEvent::BudgetExceeded { .. }) {
                tracing::debug!(session_id = %session_id, source_seq = row.seq, "H4 budget fields omitted: public ExecutionObservationV1 has no budget ceiling or current-budget field");
            }
            let Some((provider, model)) = route.as_ref() else {
                continue;
            };
            let Some(observation) = project_execution_observation(
                session_id, &task_id, scope_id, row.seq, &event, provider, model,
            ) else {
                continue;
            };
            cursor.last_input_sequence += 1;
            observation_source_tail = row.seq;
            observations
                .push(json!({"sequence":cursor.last_input_sequence,"observation":observation}));
            if observations.len() >= 128 {
                break;
            }
        }
        let source_tail = if observations.len() >= 128 {
            observation_source_tail
        } else {
            last_source_seq
        };
        if observations.is_empty() {
            let next = AdaptObservationCursor {
                last_input_sequence: initial_cursor.last_input_sequence,
                last_source_seq: source_tail,
                provider: cursor.provider,
                model: cursor.model,
            };
            let store = storage.blocking_lock();
            if let Err(error) =
                store.adapt_observation_advance(session_id, &task_id, initial_cursor, next)
            {
                tracing::warn!(session_id = %session_id, %error, "could not advance empty H4 observation cursor");
            }
            return;
        }
        let first_seq = observations
            .first()
            .and_then(|row| row["sequence"].as_u64())
            .unwrap_or(0);
        let expected_cursor = first_seq.saturating_sub(1);
        let window_id = format!(
            "coderight-h4:{session_id}:{first_seq}:{}",
            cursor.last_input_sequence
        );
        let request = json!({
            "operation":"analyze", "scope":scope_id, "windowId":window_id,
            "sessionId":session_id, "taskId":task_id,
            "expectedCursor":expected_cursor, "requiredCallIds":[], "observations":observations,
        });
        match cli.adapt_observe(&request) {
            Ok(_) => {
                let expected = initial_cursor;
                let next = AdaptObservationCursor {
                    last_input_sequence: cursor.last_input_sequence,
                    last_source_seq: source_tail,
                    provider: cursor.provider,
                    model: cursor.model,
                };
                let submitted_count = next
                    .last_input_sequence
                    .saturating_sub(expected.last_input_sequence);
                let store = storage.blocking_lock();
                match store.adapt_observation_advance(session_id, &task_id, expected, next) {
                    Ok(true) => {
                        tracing::debug!(session_id = %session_id, count = submitted_count, "H4 execution observations submitted")
                    }
                    Ok(false) => {
                        tracing::warn!(session_id = %session_id, "H4 observation cursor CAS lost; durable events will be replayed")
                    }
                    Err(error) => {
                        tracing::warn!(session_id = %session_id, %error, "H4 observation cursor persistence failed")
                    }
                }
            }
            Err(error) => {
                tracing::warn!(session_id = %session_id, %error, "authenticated H4 observation submission failed")
            }
        }
    }

    fn session_lock(&self, session_id: &str) -> Arc<Mutex<()>> {
        lock_recover(&self.session_locks, "transcript_exporter.session_locks")
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Export every durable event after the committed cursor for one
    /// session. Blocking: call inside `tokio::task::spawn_blocking` (or use
    /// [`spawn_session_export`]). Serialized per session; safe to call from
    /// any number of triggers.
    pub fn export_session(
        &self,
        storage: &SharedStorage,
        deleted_session_ids: &Arc<Mutex<HashSet<String>>>,
        session_id: &str,
    ) -> ExportOutcome {
        let session_lock = self.session_lock(session_id);
        let _guard = lock_recover(&session_lock, "transcript_exporter.session");
        self.export_session_locked(storage, deleted_session_ids, session_id)
    }

    fn export_session_locked(
        &self,
        storage: &SharedStorage,
        deleted_session_ids: &Arc<Mutex<HashSet<String>>>,
        session_id: &str,
    ) -> ExportOutcome {
        let mut exported_total = 0u64;
        let Some(cli) = self.cli.as_ref() else {
            // Pulse is not installed: nothing to export, cursor untouched.
            return ExportOutcome::UpToDate { exported: 0 };
        };
        loop {
            // A session deleted between trigger and execution exports nothing.
            if lock_recover(deleted_session_ids, "deleted_session_ids").contains(session_id) {
                return ExportOutcome::SessionRemoved;
            }
            // One lock acquisition reads the cursor, the session row (the
            // scope authority), and the durable tail, then decodes each row
            // with the same row-authoritative normalization SSE replay uses.
            let (cursor, rows) = {
                let store = storage.blocking_lock();
                let cursor = match store.transcript_export_cursor(session_id) {
                    Ok(cursor) => cursor.unwrap_or_default(),
                    Err(error) => {
                        return ExportOutcome::Failed(format!("read export cursor: {error}"));
                    }
                };
                // Absence means the tree is deleted — the forget path owns
                // the stream now.
                let workspace_root = match store.get_session(session_id) {
                    Ok(Some(row)) => row.workspace_root,
                    Ok(None) => return ExportOutcome::SessionRemoved,
                    Err(error) => {
                        return ExportOutcome::Failed(format!("read session row: {error}"));
                    }
                };
                let rows = match store.events_after(session_id, cursor.last_source_seq) {
                    Ok(rows) => rows,
                    Err(error) => {
                        return ExportOutcome::Failed(format!("replay durable events: {error}"));
                    }
                };
                let mut decoded: Vec<(i64, Option<ProtocolEvent>)> = Vec::new();
                for row in rows {
                    match store.decode_event_row(&row) {
                        Ok(decoded_event) if !decoded_event.tombstone => {
                            decoded.push((row.seq, Some(decoded_event.event)));
                        }
                        Ok(_) => decoded.push((row.seq, None)),
                        Err(error) => {
                            tracing::warn!(
                                session_id = %session_id,
                                seq = row.seq,
                                %error,
                                "skipping undecodable durable event during transcript export"
                            );
                            decoded.push((row.seq, None));
                        }
                    }
                }
                let scope_id = repository_scope_id(std::path::Path::new(&workspace_root));
                (cursor, (scope_id, decoded))
            };
            let (scope_id, decoded) = rows;

            // Project durable events only. Live projections and undecodable
            // rows never become transcript rows — they leave sourceSeq gaps,
            // which the contract explicitly permits.
            let mut batch: Vec<Value> = Vec::new();
            let mut last_source = cursor.last_source_seq;
            for (source_seq, event) in decoded {
                if source_seq <= last_source {
                    continue;
                }
                last_source = source_seq;
                let Some(event) = event else {
                    continue;
                };
                if !event.is_durable_fact() {
                    continue;
                }
                batch.push(project_event(session_id, source_seq, &event, &scope_id));
                if batch.len() >= MAX_EVENTS_PER_APPEND {
                    break;
                }
            }
            if batch.is_empty() {
                return ExportOutcome::UpToDate {
                    exported: exported_total,
                };
            }

            let batch_last_source = last_source;
            let events = Value::Array(batch);
            match cli.transcript_append(
                session_id,
                cursor.last_stream_seq as u64,
                Some(&scope_id),
                &events,
            ) {
                Ok(TranscriptAppend::Committed {
                    cursor: committed_end,
                    receipt,
                    ..
                }) => {
                    let last_seq = receipt
                        .get("lastSeq")
                        .and_then(Value::as_u64)
                        .unwrap_or(committed_end);
                    let next = TranscriptExportCursor {
                        last_stream_seq: last_seq as i64,
                        last_source_seq: batch_last_source,
                    };
                    let advanced = {
                        let store = storage.blocking_lock();
                        store.transcript_export_advance(session_id, &cursor, &next)
                    };
                    match advanced {
                        Ok(true) => {
                            let field =
                                |key: &str| receipt.get(key).and_then(Value::as_u64).unwrap_or(0);
                            exported_total +=
                                field("inserted").saturating_add(field("alreadyPresent"));
                        }
                        Ok(false) => {
                            // A writer we do not own advanced the local
                            // cursor (or forget deleted the row). Resync and
                            // rerun rather than silently diverge.
                            tracing::warn!(
                                session_id = %session_id,
                                "transcript export cursor CAS lost; resynchronizing"
                            );
                            continue;
                        }
                        Err(error) => {
                            return ExportOutcome::Failed(format!(
                                "advance export cursor: {error}"
                            ));
                        }
                    }
                }
                Ok(TranscriptAppend::Conflict(conflict)) => match conflict.code.as_str() {
                    "transcript_session_gone" => {
                        let store = storage.blocking_lock();
                        if let Err(error) = store.delete_transcript_export_cursor(session_id) {
                            tracing::warn!(
                                session_id = %session_id,
                                %error,
                                "failed to delete transcript export cursor after session-gone conflict"
                            );
                        }
                        return ExportOutcome::Gone;
                    }
                    "transcript_cursor_mismatch" => {
                        // Membrane's committed end is authoritative. Adopt it
                        // and replay from the matching source position;
                        // identical replays surface as already present,
                        // divergence as an event conflict.
                        let Some(end) = conflict.current_cursor else {
                            return ExportOutcome::Failed(
                                "transcript cursor mismatch without a committed end".into(),
                            );
                        };
                        let source = match Self::source_seq_for_stream(storage, session_id, end) {
                            Ok(source) => source,
                            Err(error) => return ExportOutcome::Failed(error),
                        };
                        let resync = {
                            let store = storage.blocking_lock();
                            store.transcript_export_set(
                                session_id,
                                &TranscriptExportCursor {
                                    last_stream_seq: end as i64,
                                    last_source_seq: source,
                                },
                            )
                        };
                        if let Err(error) = resync {
                            return ExportOutcome::Failed(format!(
                                "resync export cursor to membrane end: {error}"
                            ));
                        }
                        tracing::info!(
                            session_id = %session_id,
                            current_cursor = end,
                            last_source_seq = source,
                            "transcript export resynchronized to membrane end"
                        );
                        continue;
                    }
                    other => {
                        return ExportOutcome::Failed(format!(
                            "transcript append conflict `{other}`: {}",
                            conflict.detail
                        ));
                    }
                },
                Err(rightkit_memory::membrane::MembraneError::Unavailable(_)) => {
                    return ExportOutcome::UpToDate {
                        exported: exported_total,
                    };
                }
                Err(error) => {
                    return ExportOutcome::Failed(format!("transcript append: {error}"));
                }
            }
        }
    }

    /// Forget a deleted session's absorbed stream. Called after the local
    /// session tree is committed-deleted; best-effort, errors surface in
    /// logs (the local cursor row is already gone via `delete_session_tree`).
    pub fn forget_session(&self, _session_id: &str) -> Result<(), String> {
        Err(format!(
            "session transcript forget is {NOT_AVAILABLE_VIA_CLI}"
        ))
    }
}

/// Block until the persister has durably written through `min_seq` and the
/// publish/persist watermarks have quiesced (`published >= persisted` means
/// no durable write is in flight), bounded by `deadline_ms`. Export then
/// observes the just-committed tail rather than racing the persistence gate.
fn wait_for_durability(persistence_state: &EventPersistenceState, min_seq: i64, deadline_ms: u64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(deadline_ms);
    let mut snapshot = persistence_state.snapshot();
    loop {
        if snapshot.last_persisted_seq >= min_seq
            && snapshot.last_published_seq >= snapshot.last_persisted_seq
        {
            return;
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            tracing::warn!(
                min_seq,
                last_persisted_seq = snapshot.last_persisted_seq,
                "transcript export proceeded before persistence quiesced"
            );
            return;
        }
        snapshot = persistence_state.wait_for_change(
            snapshot,
            remaining.min(std::time::Duration::from_millis(500)),
        );
    }
}

/// Spawn a bounded export pass for `session_id`. `min_seq` is the durable
/// source seq the trigger must observe before reading (e.g. the terminal
/// turn event's seq); `0` waits only for write quiescence. No-ops when the
/// session is already deleted.
pub fn spawn_session_export(
    exporter: Arc<NativeTranscriptExporter>,
    storage: Option<SharedStorage>,
    deleted_session_ids: Arc<Mutex<HashSet<String>>>,
    persistence_state: EventPersistenceState,
    session_id: String,
    min_seq: i64,
) {
    let Some(storage) = storage else {
        return;
    };
    tokio::task::spawn_blocking(move || {
        wait_for_durability(&persistence_state, min_seq, 15_000);
        let outcome = exporter.export_session(&storage, &deleted_session_ids, &session_id);
        match outcome {
            ExportOutcome::UpToDate { exported } => {
                if exported > 0 {
                    tracing::debug!(session_id = %session_id, exported, "session transcript exported");
                }
                let scope_id =
                    {
                        let store = storage.blocking_lock();
                        store.get_session(&session_id).ok().flatten().map(|row| {
                            repository_scope_id(std::path::Path::new(&row.workspace_root))
                        })
                    };
                if let Some(scope_id) = scope_id {
                    exporter.export_execution_observations(&storage, &session_id, &scope_id);
                }
            }
            ExportOutcome::Gone => {
                tracing::info!(session_id = %session_id, "session transcript stream is forgotten; export stopped");
            }
            ExportOutcome::Failed(error) => {
                tracing::warn!(session_id = %session_id, %error, "session transcript export failed");
            }
            _ => {}
        }
    });
}

/// Spawn the `session_transcript_forget` call for every session in a deleted
/// tree. Local cursor rows are already gone via `delete_session_tree`; this
/// orders the worker-side tombstone strictly after the durable delete.
pub fn spawn_session_forget(exporter: Arc<NativeTranscriptExporter>, session_ids: Vec<String>) {
    tokio::task::spawn_blocking(move || {
        for session_id in session_ids {
            if let Err(error) = exporter.forget_session(&session_id) {
                tracing::warn!(
                    session_id = %session_id,
                    %error,
                    "session transcript forget failed"
                );
            }
        }
    });
}

/// Project one committed `ProtocolEvent` into the `coderight_event_v1`
/// normalized row the worker stores verbatim and the `membrane-transcript`
/// `coderight` adapter consumes. Row-level identity fields
/// (`sessionId`/`sourceSeq`/`eventId`) bind the row to the stream; the
/// normalized `event` object uses the frozen `adapt_event_v1` field names so
/// the existing generic learning path reads it unchanged.
fn project_event(
    session_id: &str,
    source_seq: i64,
    event: &ProtocolEvent,
    scope_id: &str,
) -> Value {
    let event_id = rightkit_agent_engine_protocol::event::stable_event_id(session_id, source_seq);
    let normalized = normalize_event(session_id, event, scope_id);
    json!({
        "type": SESSION_TRANSCRIPT_EVENT_TYPE,
        "host": SESSION_TRANSCRIPT_HOST,
        "sessionId": session_id,
        "sourceSeq": source_seq,
        "eventId": event_id,
        "occurredAt": event.created_at(),
        "scopeId": scope_id,
        "provenance": {
            "source": "coderight",
            "producer": EXPORTER_PRODUCER,
            "sessionId": session_id,
            "sourceSeq": source_seq,
            "eventId": event_id,
        },
        // The canonical event is preserved verbatim beside its projection so
        // re-anchoring, audits, and future detectors see the durable truth,
        // not just the normalized learning shape.
        "sourceEvent": serde_json::to_value(event).unwrap_or(Value::Null),
        "event": normalized,
    })
}

/// Scope id for a workspace: the path with separators and other non
/// alphanumerics collapsed to `-` (a `D:` drive path `D:\repo` becomes
/// `D--repo`). Local to the exporter now that the Hub canonical-repository
/// helper is gone.
/// Byte-identical copy of membrane_federation::canonical_repository_id
/// (context/engine/crates/membrane-federation/src/root.rs:148) so existing
/// memory scopes stay addressable after the Membrane CLI cutover.
fn repository_scope_id(path: &std::path::Path) -> String {
    let mut value = path.to_string_lossy().replace('\\', "/");
    if let Some(rest) = value.strip_prefix("//?/") {
        value = rest.to_owned();
    }
    value = value.replace(':', "-").replace('/', "-");
    let value = value.trim_matches('-');
    if value.is_empty() {
        return "global".to_owned();
    }
    let bytes = value.as_bytes();
    if bytes.len() > 1 && bytes[0].is_ascii_lowercase() && bytes[1] == b'-' {
        let mut normalized = value.to_owned();
        normalized.replace_range(..1, &value[..1].to_ascii_uppercase());
        normalized
    } else {
        value.to_owned()
    }
}

/// H4 projection from canonical durable events. Only dispatch metadata, route
/// identities, agent roles and provider-reported usage cross this seam.
fn project_execution_observation(
    session_id: &str,
    task_id: &str,
    scope: &str,
    source_seq: i64,
    event: &ProtocolEvent,
    provider: &str,
    model: &str,
) -> Option<Value> {
    let timestamp = parse_unix_ms(event.created_at());
    if timestamp == 0 {
        return None;
    }
    let mut kind = "route_selected";
    let mut subject = None;
    let mut role = None;
    let mut usage = None;
    match event {
        ProtocolEvent::ProviderRouteUpdated {
            provider: actual_provider,
            model: actual_model,
            ..
        } if !actual_provider.trim().is_empty() && !actual_model.trim().is_empty() => {
            subject = Some(format!("{actual_provider}/{actual_model}"));
        }
        ProtocolEvent::TurnUsage {
            input_tokens,
            cached_input_tokens,
            cache_creation_tokens,
            output_tokens,
            ..
        } => {
            kind = "model_call_finished";
            let token = |value: i64| match u64::try_from(value) {
                Ok(value) => json!({"coverage":"complete","value":value}),
                Err(_) => json!({"coverage":"unavailable","unavailableReason":"provider_omitted"}),
            };
            usage = Some(json!({
                "coverage":"complete",
                "value":{
                    "inputTokens":{"basis":{"id":"provider_reported","version":"1"},"estimate":token(*input_tokens)},
                    "outputTokens":{"basis":{"id":"provider_reported","version":"1"},"estimate":token(*output_tokens)},
                    "cacheReadInputTokens":token(*cached_input_tokens),
                    "cacheWriteInputTokens":token(*cache_creation_tokens),
                }
            }));
            subject = Some(format!("provider-response:{source_seq}"));
        }
        ProtocolEvent::DispatchDecided { strategy, .. } => {
            kind = "plan_created";
            subject = Some(strategy.clone());
        }
        ProtocolEvent::AgentQueryIssued {
            query_id, to_role, ..
        } => {
            kind = "subagent_started";
            subject = Some(query_id.clone());
            if !to_role.trim().is_empty() {
                role = Some(to_role.clone());
            }
        }
        _ => return None,
    }
    let encoded = serde_json::to_vec(event).ok()?;
    let digest = format!(
        "sha256:{}",
        rightkit_fs::digest::sha256_bytes(AsRef::<[u8]>::as_ref(&encoded))
    );
    let unavailable = json!({"coverage":"unavailable","unavailableReason":"provider_omitted"});
    Some(json!({
        "schemaVersion":1,
        "observationId":format!("coderight-h4:{session_id}:{source_seq}"),
        "sessionId":session_id,
        "taskId":{"coverage":"complete","value":task_id},
        "parentTaskId":unavailable,
        "agentId":unavailable,
        "agentRole":role.map(|value| json!({"coverage":"complete","value":value})).unwrap_or_else(|| json!({"coverage":"unavailable","unavailableReason":"host_unsupported"})),
        "observedAtUnixMs":timestamp,
        "model":model,
        "provider":provider,
        "client":"coderight",
        "routePolicy":unavailable,
        "observationKind":kind,
        "subjectId":subject.map(|value| json!({"coverage":"complete","value":value})).unwrap_or_else(|| unavailable.clone()),
        "tool":unavailable,
        "callId":unavailable,
        "outcome":unavailable,
        "exitCode":unavailable,
        "durationMs":unavailable,
        "usage":usage.unwrap_or_else(|| unavailable.clone()),
        "toolCost":unavailable,
        "assetCost":unavailable,
        "repository":{"coverage":"complete","value":scope},
        "scope":{"coverage":"complete","value":scope},
        "artifactRefs":{"coverage":"complete","value":[]},
        "evidenceRefs":{"coverage":"complete","value":[]},
        "completion":unavailable,
        "provenanceReceipt":{
            "schemaVersion":1,
            "receiptId":format!("coderight-event:{session_id}:{source_seq}"),
            "source":"coderight",
            "observedAtUnixMs":timestamp,
            "receiptDigest":digest
        }
    }))
}

/// Map a durable `ProtocolEvent` to the frozen generic event shape
/// (`kind`/`role`/`text`/`tool`/`call_id`/…). Only the semantically
/// meaningful variants project onto learning kinds; everything else lands as
/// a `meta` row so the absorbed stream keeps full session truth without
/// teaching Adapt from control noise.
fn normalize_event(session_id: &str, event: &ProtocolEvent, scope_id: &str) -> Value {
    let timestamp = event.created_at();
    let kind = serde_json::to_value(event)
        .ok()
        .and_then(|value| {
            value
                .get("$kind")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".to_string());
    let base = json!({
        "kind": "meta",
        "meta": true,
        "timestamp": timestamp,
        "sessionId": session_id,
        "repo": scope_id,
        "eventKind": kind,
    });
    let mut obj = base.as_object().cloned().unwrap_or_default();
    match event {
        ProtocolEvent::UserMessageSubmitted { text, .. } => {
            obj.insert("kind".into(), json!("user_message"));
            obj.insert("role".into(), json!("user"));
            obj.insert("text".into(), json!(text));
            obj.remove("meta");
        }
        ProtocolEvent::HumanSteeringInjected {
            injected_by,
            content_preview,
            text,
            ..
        } => {
            // Steering is the user's message mid-turn; it is the strongest
            // human-preference signal the session emits, so it normalizes as
            // a user_message. `None` (pre-field rows) keeps the preview.
            obj.insert("kind".into(), json!("user_message"));
            obj.insert("role".into(), json!("user"));
            obj.insert(
                "text".into(),
                json!(text.clone().unwrap_or_else(|| content_preview.clone())),
            );
            obj.insert("injectedBy".into(), json!(injected_by));
            obj.remove("meta");
        }
        ProtocolEvent::AssistantMessageCompleted { message, .. } => {
            if let TranscriptMessage::Assistant {
                content,
                reasoning_content,
                ..
            } = message
            {
                obj.insert("kind".into(), json!("assistant_message"));
                obj.insert("role".into(), json!("assistant"));
                obj.insert("text".into(), json!(content));
                if reasoning_content.is_some() {
                    obj.insert("private_reasoning_omitted".into(), json!(true));
                }
                obj.remove("meta");
            }
        }
        ProtocolEvent::ToolCallStarted { call, .. } => {
            obj.insert("kind".into(), json!("tool_call"));
            obj.insert("tool".into(), json!(call.name));
            obj.insert("call_id".into(), json!(call.id));
            obj.insert("text".into(), json!(call.arguments_json));
            obj.remove("meta");
        }
        ProtocolEvent::ToolResultAdded { result, .. } => {
            if let TranscriptMessage::Tool {
                tool_call_id,
                summary,
                output,
                is_error,
                ..
            } = result
            {
                obj.insert("kind".into(), json!("tool_result"));
                obj.insert("call_id".into(), json!(tool_call_id));
                obj.insert("is_error".into(), json!(is_error));
                // `output` is an OutputRef to the externalized payload; the
                // stored preview is the bounded learning text, `summary` the
                // fallback when no preview was persisted.
                obj.insert(
                    "text".into(),
                    json!(output
                        .as_ref()
                        .and_then(|reference| reference.preview.clone())
                        .unwrap_or_else(|| summary.clone())),
                );
                if output.as_ref().is_some_and(|reference| reference.truncated) {
                    obj.insert("output_truncated".into(), json!(true));
                }
                obj.remove("meta");
            }
        }
        _ => {}
    }
    Value::Object(obj)
}

/// Best-effort `occurredAt` conversion. RightKit `created_at` values are
/// ISO-8601 (`YYYY-MM-DDTHH:MM:SSZ`) or raw epoch seconds depending on the
/// writer; anything else degrades to `0` rather than failing the row.
fn parse_unix_ms(created_at: &str) -> u64 {
    let trimmed = created_at.trim();
    if let Ok(secs) = trimmed.parse::<u64>() {
        return secs.saturating_mul(1000);
    }
    // Fixed-shape ISO-8601 UTC: YYYY-MM-DDTHH:MM:SSZ (fractional seconds and
    // zone offsets fall through to 0 — no RightKit writer produces them).
    let bytes = trimmed.as_bytes();
    if bytes.len() == 20 && bytes[19] == b'Z' {
        if let Some(parsed) = rightkit_time::DateTime::parse_rfc3339(trimmed) {
            if parsed.second <= 59 {
                return u64::try_from(parsed.unix_seconds().saturating_mul(1000)).unwrap_or(0);
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_engine_protocol::event::stable_event_id;

    fn session_event(seq: i64) -> ProtocolEvent {
        ProtocolEvent::UserMessageSubmitted {
            session_id: "s-1".into(),
            seq,
            created_at: "2026-06-08T00:00:10Z".into(),
            text: "fix the parser".into(),
            nonce: None,
            trust: None,
            attachments: Vec::new(),
        }
    }

    #[test]
    fn steering_event_projects_full_text_as_user_message() {
        let event = ProtocolEvent::HumanSteeringInjected {
            session_id: "s-1".into(),
            seq: 9,
            created_at: "2026-06-08T00:00:11Z".into(),
            injected_by: "steer".into(),
            content_preview: "prefer the safer path".into(),
            text: Some("prefer the safer path, keep the public API".into()),
        };
        let row = project_event("s-1", 9, &event, "D--repo");
        assert_eq!(row["eventId"], json!(stable_event_id("s-1", 9)));
        assert_eq!(row["sourceSeq"], json!(9));
        assert_eq!(row["type"], json!("coderight_event_v1"));
        assert_eq!(row["host"], json!("coderight"));
        assert_eq!(row["provenance"]["source"], json!("coderight"));
        let projected = &row["event"];
        assert_eq!(projected["kind"], json!("user_message"));
        assert_eq!(projected["role"], json!("user"));
        assert_eq!(
            projected["text"],
            json!("prefer the safer path, keep the public API")
        );
        assert_eq!(projected["injectedBy"], json!("steer"));
        // The canonical event rides beside the projection verbatim.
        assert_eq!(row["sourceEvent"]["$kind"], json!("humanSteeringInjected"));
    }

    #[test]
    fn user_turn_projects_as_user_message() {
        let row = project_event("s-1", 4, &session_event(4), "D--repo");
        let projected = &row["event"];
        assert_eq!(projected["kind"], json!("user_message"));
        assert_eq!(projected["text"], json!("fix the parser"));
        assert_eq!(projected["sessionId"], json!("s-1"));
        assert_eq!(projected["repo"], json!("D--repo"));
        assert!(parse_unix_ms(row["occurredAt"].as_str().unwrap()) > 0);
    }

    #[test]
    fn tool_call_and_result_project() {
        let call = ProtocolEvent::ToolCallStarted {
            session_id: "s-1".into(),
            seq: 5,
            created_at: "t".into(),
            call: rightkit_agent_engine_protocol::ToolCall {
                id: "tc-1".into(),
                name: "read_file".into(),
                arguments_json: "{\"path\":\"a\"}".into(),
            },
        };
        let row = project_event("s-1", 5, &call, "D--repo");
        assert_eq!(row["event"]["kind"], json!("tool_call"));
        assert_eq!(row["event"]["tool"], json!("read_file"));
        assert_eq!(row["event"]["call_id"], json!("tc-1"));

        let result = ProtocolEvent::ToolResultAdded {
            session_id: "s-1".into(),
            seq: 6,
            created_at: "t".into(),
            result: TranscriptMessage::Tool {
                id: "r".into(),
                created_at: "t".into(),
                tool_call_id: "tc-1".into(),
                summary: "ok".into(),
                output: Some(rightkit_agent_engine_protocol::OutputRef {
                    id: "out-1".into(),
                    kind: "tool_output".into(),
                    size_bytes: 8,
                    content_hash: None,
                    preview: Some("contents".into()),
                    media_type: "text/plain; charset=utf-8".into(),
                    truncated: false,
                    retrievable: true,
                }),
                trust: None,
                is_error: false,
                pinned: false,
                process_id: None,
            },
        };
        let row = project_event("s-1", 6, &result, "D--repo");
        assert_eq!(row["event"]["kind"], json!("tool_result"));
        assert_eq!(row["event"]["text"], json!("contents"));
        assert_eq!(row["event"]["is_error"], json!(false));
    }

    #[test]
    fn control_events_project_as_meta_rows() {
        let event = ProtocolEvent::SessionTurnFinished {
            session_id: "s-1".into(),
            seq: 20,
            created_at: "t".into(),
            turn_id: "turn-20".into(),
            status: "completed".into(),
        };
        let row = project_event("s-1", 20, &event, "D--repo");
        assert_eq!(row["event"]["kind"], json!("meta"));
        assert_eq!(row["event"]["meta"], json!(true));
        assert_eq!(row["event"]["eventKind"], json!("sessionTurnFinished"));
    }

    #[test]
    fn occurred_at_parses_epoch_seconds_and_iso() {
        assert_eq!(parse_unix_ms("1704067200"), 1_704_067_200_000);
        assert_eq!(parse_unix_ms("2024-01-01T00:00:00Z"), 1_704_067_200_000);
        assert_eq!(parse_unix_ms("2026-06-08T00:00:10Z"), 1_780_876_810_000);
        assert_eq!(parse_unix_ms("not-a-time"), 0);
        assert_eq!(parse_unix_ms(""), 0);
    }
}
