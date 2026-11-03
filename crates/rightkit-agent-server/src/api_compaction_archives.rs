//! Authenticated recovery surface and durable sink for exact pre-compaction transcripts.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::get,
    Router,
};
use rightkit_agent_engine_core::compaction_archive::{
    serialize_archive_messages, CompactionArchiveError,
};
use rightkit_agent_engine_core::{
    CompactionArchiveInput, CompactionArchiveReceipt, CompactionArchiveSink,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::api::{DaemonState, SharedStorage};
use crate::api_permissions::enforce_trusted_source_authority;

static ARCHIVE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub(crate) struct SqliteCompactionArchiveSink {
    storage: SharedStorage,
    snapshot_feed:
        std::sync::Arc<std::sync::RwLock<rightkit_agent_engine_core::CompactionSnapshotV1>>,
}

impl SqliteCompactionArchiveSink {
    pub(crate) fn new(storage: SharedStorage) -> Self {
        Self {
            storage,
            snapshot_feed: std::sync::Arc::new(std::sync::RwLock::new(
                rightkit_agent_engine_core::CompactionSnapshotV1 {
                    request_generation: 0,
                    transcript_high_water: 0,
                    transcript_digest: String::new(),
                    pin_revision: None,
                    model_route_revision: None,
                    tool_catalog_revision: None,
                    promotion_revision: None,
                    capacity_revision: None,
                    reserve_revision: None,
                    accounting_revision: None,
                    federation_revision: None,
                    binding_revision: None,
                    h9_revision: None,
                    policy_revision: None,
                    repository_scope_revision: None,
                    decision_expires_at_unix_ms: 0,
                },
            )),
        }
    }

    pub(crate) fn with_snapshot_feed(
        mut self,
        feed: std::sync::Arc<std::sync::RwLock<rightkit_agent_engine_core::CompactionSnapshotV1>>,
    ) -> Self {
        self.snapshot_feed = feed;
        self
    }
}

#[async_trait::async_trait]
impl CompactionArchiveSink for SqliteCompactionArchiveSink {
    async fn archive(
        &self,
        input: CompactionArchiveInput,
    ) -> Result<CompactionArchiveReceipt, CompactionArchiveError> {
        input.validate()?;
        let federation_key = if let Some(decision) = input.federation_decision.as_ref() {
            if input.snapshot.is_none() {
                return Err(CompactionArchiveError::new(
                    rightkit_agent_engine_core::CompactionFailureClassV1::FederationUnavailable,
                    "compaction federation decision has no snapshot",
                ));
            }
            if decision.schema != rightkit_agent_engine_protocol::COMPACTION_FEDERATION_SCHEMA_V1
                || decision.decision != "approved"
                || decision.identity.trim().is_empty()
                || decision.fence.trim().is_empty()
                || decision.scope.trim().is_empty()
                || decision.selection_receipt.selection_hash.trim().is_empty()
            {
                return Err(CompactionArchiveError::new(
                    rightkit_agent_engine_core::CompactionFailureClassV1::FederationUnavailable,
                    "compaction federation decision refused: invalid canonical authorization",
                ));
            }
            let received = input.federation_request_started_at.ok_or_else(|| {
                CompactionArchiveError::new(
                    rightkit_agent_engine_core::CompactionFailureClassV1::FederationUnavailable,
                    "compaction federation decision refused: receipt age unavailable",
                )
            })?;
            let age_ms = received.elapsed().as_millis() as u64;
            if age_ms > decision.expires_after_elapsed_ms {
                return Err(CompactionArchiveError::new(
                    rightkit_agent_engine_core::CompactionFailureClassV1::FederationUnavailable,
                    "compaction federation decision refused: expired",
                ));
            }
            if input
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.binding_revision)
                != Some(decision.restart_epoch)
            {
                return Err(CompactionArchiveError::new(
                    rightkit_agent_engine_core::CompactionFailureClassV1::StaleSnapshot,
                    "compaction federation decision refused: restart epoch mismatch",
                ));
            }
            Some((decision.identity.as_str(), decision.restart_epoch))
        } else {
            None
        };
        // Use engine's canonical byte serializer so durable archive bytes,
        // receipt hashes, & recovery verification share one encoding path.
        let transcript_json = serialize_archive_messages(&input.messages)?;
        let archive_id = format!(
            "compact-{}-{}-{}",
            std::process::id(),
            crate::api::now_millis(),
            ARCHIVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let mut storage = self.storage.lock().await;

        // CR-16: re-hash the projection record the live compaction path
        // built, so the hourly retention job can prefer a record captured at
        // the true moment of compaction over one derived operationally two
        // ticks later.
        //
        // The carried record's `content_hash` is deliberately NOT the hash we
        // store. The live path hashes the transcript it holds in memory --
        // masked tool output, live message ids -- while the retention job
        // verifies against `rehydrate_transcript(events_in_range(..))`, read
        // back from canonical event rows. Those two representations of the same
        // conversation do not serialize identically, so a verbatim copy of the
        // live hash could never match on the verifying side. Retention would
        // then refuse every deletion forever: safe, but permanently stuck, and
        // indistinguishable from the feature working.
        //
        // So the range and the summary come from the record, and the hash is
        // recomputed here over exactly the basis the verifier will use. This
        // read happens before `commit_compaction`'s transaction opens -- it
        // needs `events_in_range` plus `rehydrate_transcript`, which live
        // above the storage crate -- but it is still race-free: `storage` is
        // held locked (the daemon's single-writer mutex) across this whole
        // read-then-commit sequence, so no other writer can interleave
        // between the rehash and the atomic commit below. A rehash/encode
        // failure aborts before any archive or companion event is written.
        let projection_payload = input
            .projection
            .as_ref()
            .map(|projection| {
                let record = rehash_over_canonical_rows(&storage, projection)
                    .map_err(|error| format!("projection not re-hashed: {error}"))?;
                serde_json::to_string(&record)
                    .map_err(|error| format!("projection not encodable: {error}"))
            })
            .transpose()?;
        let projection_event = projection_payload.as_deref().map(|payload| {
            (
                rightkit_agent_engine_core::compaction_archive::PROJECTION_EVENT_KIND,
                payload,
            )
        });

        // FM-178 / CRA-11 (Z24 residual fix): encode lineage, if the caller
        // handed one over, BEFORE the transaction opens -- same pattern as
        // the projection payload above -- so a bad encode aborts before
        // any write rather than partially committing. Once encoded, the
        // event is passed into the SAME `commit_compaction` call as the
        // archive and projection, so it commits inside the one
        // Storage-owned transaction below. There is no second
        // `self.storage.lock().await` and no separate best-effort write:
        // the previous shape (a standalone `persist_lineage` call with its
        // own lock, swallowing its own errors via `eprintln!`) was exactly
        // the hidden-partial-commit defect this closes -- a crash between
        // the two independent commits could leave a durable archive with no
        // lineage.
        let lineage_payload = match input.lineage.as_ref() {
            Some(lineage) => Some(
                serde_json::to_string(lineage)
                    .map_err(|error| format!("lineage not encodable: {error}"))?,
            ),
            None => None,
        };
        let lineage_event = lineage_payload.as_deref().map(|payload| {
            (
                rightkit_agent_engine_core::compaction_archive::LINEAGE_EVENT_KIND,
                payload,
            )
        });

        // CRA-11: archive, projection event, lineage event, durable
        // compaction_epoch advance, and sessionCompacted event commit
        // together inside one Storage-owned transaction. A failure here
        // rolls back the whole set -- there is no partial state where the
        // archive lands but the epoch or lineage does not, or vice versa.
        let expected_snapshot = input.snapshot.clone();
        let expected_staged_events =
            projection_event.is_some() as i64 + lineage_event.is_some() as i64 + 1; // sessionCompacted marker
                                                                                    // Acquire after the async storage lock, then keep this authority guard
                                                                                    // through the synchronous transaction and COMMIT. No producer can
                                                                                    // change the validated snapshot between the callback and durability.
        let live_snapshot = self
            .snapshot_feed
            .read()
            .map_err(|_| "compaction snapshot feed poisoned".to_string())?;
        let validator = |tx: &rightkit_db::rusqlite::Transaction<'_>| {
            if let Some(snapshot) = expected_snapshot.as_ref() {
                if !snapshot.failure_relevant_eq(&live_snapshot) {
                    return Err(rightkit_agent_engine_storage::StorageError::StaleSnapshot);
                }
                // Engine-observed authorities are always required. Host-owned
                // authorities are required only when they exist for this
                // session: federation/binding when a federation decision rides
                // this archive; the model-route and H9 retention revisions are
                // absent by design for direct/degraded ("context off") sessions
                // (D18) and are still compared by `failure_relevant_eq` above.
                let federated = input.federation_decision.is_some();
                if [
                    snapshot.pin_revision,
                    snapshot.tool_catalog_revision,
                    snapshot.promotion_revision,
                    snapshot.capacity_revision,
                    snapshot.reserve_revision,
                    snapshot.accounting_revision,
                    snapshot.policy_revision,
                    snapshot.repository_scope_revision,
                ]
                .iter()
                .any(Option::is_none)
                    || (federated
                        && (snapshot.federation_revision.is_none()
                            || snapshot.binding_revision.is_none()))
                {
                    return Err(rightkit_agent_engine_storage::StorageError::Serde(
                        "required compaction revision unavailable".into(),
                    ));
                }
                if snapshot.decision_expires_at_unix_ms != u64::MAX
                    && crate::api::now_millis() >= u128::from(snapshot.decision_expires_at_unix_ms)
                {
                    return Err(rightkit_agent_engine_storage::StorageError::StaleSnapshot);
                }
                if let Some(decision) = input.federation_decision.as_ref() {
                    let received = input.federation_request_started_at.ok_or_else(|| {
                        rightkit_agent_engine_storage::StorageError::StaleSnapshot
                    })?;
                    if received.elapsed().as_millis() as u64 > decision.expires_after_elapsed_ms
                        || snapshot.binding_revision != Some(decision.restart_epoch)
                    {
                        return Err(rightkit_agent_engine_storage::StorageError::StaleSnapshot);
                    }
                }
                let current_high_water: i64 = tx
                    .query_row(
                        "SELECT COALESCE(MAX(seq), 0) FROM events WHERE session_id = ?1",
                        [&input.session_id],
                        |row| row.get(0),
                    )
                    .map_err(rightkit_agent_engine_storage::StorageError::Sqlite)?;
                let expected_final_high_water = snapshot
                    .transcript_high_water
                    .saturating_add(expected_staged_events);
                if current_high_water != expected_final_high_water {
                    return Err(rightkit_agent_engine_storage::StorageError::StaleSnapshot);
                }
            }
            Ok(())
        };
        let result = if let Some(key) = federation_key {
            storage.commit_compaction_with_decision(
                &archive_id,
                &input.session_id,
                &input.created_at,
                &transcript_json,
                input.messages.len(),
                projection_event,
                lineage_event,
                key,
                validator,
            )
        } else {
            storage.commit_compaction_with_validation(
                &archive_id,
                &input.session_id,
                &input.created_at,
                &transcript_json,
                input.messages.len(),
                projection_event,
                lineage_event,
                validator,
            )
        };
        let (row, epoch) = result.map_err(|error| {
            let class = match &error {
                rightkit_agent_engine_storage::StorageError::StaleSnapshot => {
                    rightkit_agent_engine_core::CompactionFailureClassV1::StaleSnapshot
                }
                _ => rightkit_agent_engine_core::CompactionFailureClassV1::StorageUnavailable,
            };
            CompactionArchiveError::new(class, error.to_string())
        })?;
        drop(live_snapshot);

        Ok(CompactionArchiveReceipt {
            archive_id: row.id,
            sha256: row.sha256,
            byte_length: row.byte_length,
            message_count: row.message_count,
            compaction_epoch: u64::try_from(epoch).unwrap_or_default(),
            durable_high_water: storage.latest_seq(&input.session_id).map_err(|error| {
                CompactionArchiveError::new(
                    rightkit_agent_engine_core::CompactionFailureClassV1::StorageUnavailable,
                    error.to_string(),
                )
            })?,
        })
    }
}

/// Rebuild `projection` with its content hash recomputed over the canonical
/// event rows it references, rather than over the in-memory transcript the
/// live compaction path hashed.
///
/// This is the whole reason the sink does not simply store what it was handed.
/// `run_event_retention_pass` verifies a record by fetching
/// `events_in_range(session, 0, source_seq_end)`, rehydrating it, and hashing
/// that -- so the stored hash must be computed the same way or verification can
/// never succeed. Returning `Err` leaves retention on its operational fallback,
/// which is correct but slower; it never deletes anything on a bad hash.
fn rehash_over_canonical_rows(
    storage: &rightkit_agent_engine_storage::Storage,
    projection: &rightkit_agent_engine_core::compaction_archive::CompactionProjectionRecord,
) -> Result<rightkit_agent_engine_core::compaction_archive::CompactionProjectionRecord, String> {
    let rows = storage
        .events_in_range(
            &projection.session_id,
            projection.source_seq_start as i64,
            projection.source_seq_end as i64,
            usize::MAX,
        )
        .map_err(|error| error.to_string())?;
    if rows.is_empty() {
        return Err(format!(
            "no canonical rows in [{}, {}]",
            projection.source_seq_start, projection.source_seq_end
        ));
    }
    let canonical_range = crate::rehydrate::rehydrate_transcript(&rows);
    Ok(
        rightkit_agent_engine_core::compaction_archive::CompactionProjectionRecord::build(
            projection.session_id.clone(),
            projection.source_seq_start,
            projection.source_seq_end,
            &canonical_range,
            projection.summary.clone(),
        ),
    )
}

pub(crate) fn compaction_archive_routes() -> Router<DaemonState> {
    Router::new()
        .route(
            "/sessions/{id}/compaction-archives",
            get(list_compaction_archives),
        )
        .route(
            "/sessions/{id}/compaction-archives/{archive_id}",
            get(get_compaction_archive),
        )
}

async fn list_compaction_archives(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let storage = state.storage.as_ref().ok_or(StatusCode::NOT_FOUND)?;
    let rows = storage
        .lock()
        .await
        .list_compaction_archives(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({
        "sessionId": id,
        "archives": rows.into_iter().map(metadata_json).collect::<Vec<_>>(),
    })))
}

async fn get_compaction_archive(
    State(state): State<DaemonState>,
    Path((id, archive_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let storage = state.storage.as_ref().ok_or(StatusCode::NOT_FOUND)?;
    let row = storage
        .lock()
        .await
        .get_compaction_archive(&id, &archive_id)
        .map_err(|error| match error {
            rightkit_agent_engine_storage::StorageError::NotFound(_) => StatusCode::NOT_FOUND,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        })?;
    let messages: Vec<rightkit_agent_engine_protocol::TranscriptMessage> =
        serde_json::from_slice(row.transcript_json.as_deref().unwrap_or_default())
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({
        "id": row.id,
        "sessionId": row.session_id,
        "createdAt": row.created_at,
        "sha256": row.sha256,
        "byteLength": row.byte_length,
        "messageCount": row.message_count,
        "messages": messages,
    })))
}

fn metadata_json(row: rightkit_agent_engine_storage::CompactionArchiveRow) -> Value {
    json!({
        "id": row.id,
        "sessionId": row.session_id,
        "createdAt": row.created_at,
        "sha256": row.sha256,
        "byteLength": row.byte_length,
        "messageCount": row.message_count,
    })
}

pub(crate) fn compaction_archive_openapi_components() -> serde_json::Map<String, Value> {
    let mut schemas = serde_json::Map::new();
    schemas.insert(
        "CompactionArchiveMetadata".into(),
        json!({
            "type": "object",
            "required": ["id", "sessionId", "createdAt", "sha256", "byteLength", "messageCount"],
            "properties": {
                "id": {"type": "string"},
                "sessionId": {"type": "string"},
                "createdAt": {"type": "string"},
                "sha256": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
                "byteLength": {"type": "integer", "minimum": 1},
                "messageCount": {"type": "integer", "minimum": 1}
            }
        }),
    );
    schemas.insert(
        "CompactionArchiveReceipt".into(),
        json!({
            "type": "object",
            "required": ["id", "sha256", "byteLength", "messageCount"],
            "properties": {
                "id": {"type": "string"},
                "sha256": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
                "byteLength": {"type": "integer", "minimum": 1},
                "messageCount": {"type": "integer", "minimum": 1}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CompactionModelPolicy".into(),
        json!({
            "type": "object",
            "required": ["modelId", "capacityKnown", "contextWindowTokens", "triggerPercent", "reserveTokens", "summaryMaxOutputTokens"],
            "properties": {
                "modelId": {"type": "string"},
                "capacityKnown": {"type": "boolean"},
                "contextWindowTokens": {"type": ["integer", "null"], "minimum": 1},
                "triggerPercent": {"type": ["integer", "null"], "minimum": 1, "maximum": 95},
                "reserveTokens": {"type": ["integer", "null"], "minimum": 1},
                "summaryMaxOutputTokens": {"type": ["integer", "null"], "minimum": 1}
            },
            "additionalProperties": false
        }),
    );
    schemas
}

pub(crate) fn compaction_archive_list_response_schema() -> Value {
    json!({
        "type": "object",
        "required": ["sessionId", "archives"],
        "properties": {
            "sessionId": {"type": "string"},
            "archives": {
                "type": "array",
                "items": {"$ref": "#/components/schemas/CompactionArchiveMetadata"}
            }
        }
    })
}

pub(crate) fn compaction_archive_response_schema() -> Value {
    json!({
        "allOf": [
            {"$ref": "#/components/schemas/CompactionArchiveMetadata"},
            {
                "type": "object",
                "required": ["messages"],
                "properties": {
                    "messages": {"type": "array", "items": {"type": "object"}}
                }
            }
        ]
    })
}
