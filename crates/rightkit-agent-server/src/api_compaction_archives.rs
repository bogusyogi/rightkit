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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// Two pre-compaction user turns, mirroring `main_runtime_server`'s
    /// retention fixture so both sides of CR-16 are exercised on the same
    /// canonical shape.
    fn seed_canonical_range(store: &mut rightkit_agent_engine_storage::Storage, session_id: &str) {
        for text in ["hello", "world"] {
            store
                .append_event(
                    session_id,
                    "userMessageSubmitted",
                    &format!(
                        r#"{{"sessionId":"{session_id}","seq":0,"createdAt":"t","text":"{text}","attachments":[]}}"#
                    ),
                )
                .unwrap();
        }
    }

    /// The defect this guards against is silent and permanent: a projection
    /// stored with the live path's in-memory hash can never verify against
    /// canonical rows, so retention refuses every deletion forever while
    /// looking exactly like a working integrity check.
    ///
    /// Asserts both halves -- that the naive hash really would not have
    /// verified, and that the re-hashed record does.
    #[test]
    fn a_persisted_projection_verifies_against_the_canonical_rows_it_references() {
        let mut store = rightkit_agent_engine_storage::Storage::open_in_memory().unwrap();
        seed_canonical_range(&mut store, "sess-1");

        let canonical_range = crate::rehydrate::rehydrate_transcript(
            &store.events_in_range("sess-1", 0, 1, usize::MAX).unwrap(),
        );

        // What the live compaction path hands the sink: same conversation,
        // but hashed over the transcript it holds in memory. Masked tool
        // output and live message ids make this a different serialization.
        let live_side = vec![rightkit_agent_engine_protocol::TranscriptMessage::User {
            id: "live-id-not-the-persisted-one".into(),
            created_at: "t".into(),
            content: "hello".into(),
            trust: None,
            pinned: false,
            attachments: Vec::new(),
        }];
        let carried =
            rightkit_agent_engine_core::compaction_archive::CompactionProjectionRecord::build(
                "sess-1",
                0,
                1,
                &live_side,
                "live summary",
            );
        assert!(
            !carried.verify(&canonical_range),
            "precondition: storing the live hash verbatim must NOT verify -- if it did, \
             this test would be proving nothing and the re-hash would be pointless"
        );

        let persisted = rehash_over_canonical_rows(&store, &carried).unwrap();

        assert!(
            persisted.verify(&canonical_range),
            "the re-hashed record must verify against the rows retention reads back, \
             or retention refuses every deletion forever"
        );
        assert_eq!(persisted.source_seq_start, 0);
        assert_eq!(persisted.source_seq_end, 1);
        assert_eq!(
            persisted.summary, "live summary",
            "only the hash is recomputed; the range and summary stay as compaction recorded them"
        );
    }

    /// End-to-end across the two hash bases, through `archive()` itself
    /// rather than the helper.
    ///
    /// The live compaction path hashes `self.transcript.messages()` -- an
    /// in-memory transcript. Retention verifies against
    /// `rehydrate_transcript(events_in_range(..))` -- rows read back from
    /// storage. Every other test on this path builds its comparison operand
    /// the same way on both sides, so none of them can observe a divergence
    /// between those two representations. This one deliberately supplies a
    /// live-basis record and then verifies the *persisted* result the way
    /// `load_persisted_projection` will read it.
    ///
    /// If the sink ever stops re-hashing, this fails. If it silently stored
    /// the live hash, retention would refuse every deletion forever while
    /// reporting a healthy integrity check -- which no other test here would
    /// notice.
    #[tokio::test]
    async fn archive_persists_a_projection_that_verifies_against_rehydrated_rows() {
        use rightkit_agent_engine_core::compaction_archive::{
            CompactionProjectionRecord, PROJECTION_EVENT_KIND,
        };

        let storage =
            SharedStorage::new(rightkit_agent_engine_storage::Storage::open_in_memory().unwrap());
        {
            let mut store = storage.lock().await;
            // `commit_compaction` fail-closes when the session row is
            // missing (see `lineage_never_commits_independently_of_a_failed_archive`
            // below, which relies on exactly that). This test archives
            // against a real commit, so the session must exist first.
            store
                .create_session(&rightkit_agent_engine_storage::SessionRow {
                    id: "sess-e2e".into(),
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
            seed_canonical_range(&mut store, "sess-e2e");
        }

        // What compaction actually hands the sink: hashed over the in-memory
        // transcript, whose ids and masked content differ from the stored rows.
        let live_messages = vec![rightkit_agent_engine_protocol::TranscriptMessage::User {
            id: "live-only-id".into(),
            created_at: "t".into(),
            content: "hello".into(),
            trust: None,
            pinned: false,
            attachments: Vec::new(),
        }];
        let carried =
            CompactionProjectionRecord::build("sess-e2e", 0, 1, &live_messages, "summary");

        let sink = SqliteCompactionArchiveSink::new(storage.clone());
        sink.archive(CompactionArchiveInput {
            session_id: "sess-e2e".into(),
            created_at: "t".into(),
            messages: live_messages,
            projection: Some(carried.clone()),
            lineage: None,
            snapshot: None,
            federation_decision: None,
            federation_request_started_at: None,
        })
        .await
        .expect("archive must succeed");

        // Read it back exactly as `load_persisted_projection` does.
        let store = storage.lock().await;
        let persisted = store
            .all_events("sess-e2e")
            .unwrap()
            .into_iter()
            .filter(|row| row.kind == PROJECTION_EVENT_KIND)
            .filter_map(|row| CompactionProjectionRecord::decode_json(&row.payload_json).ok())
            .next_back()
            .expect("archive must persist a projection event");

        let canonical_range = crate::rehydrate::rehydrate_transcript(
            &store.events_in_range("sess-e2e", 0, 1, usize::MAX).unwrap(),
        );

        assert_ne!(
            persisted.content_hash, carried.content_hash,
            "the sink must NOT persist the live hash verbatim -- if these are equal the \
             re-hash was skipped and retention can never verify"
        );
        assert!(
            persisted.verify(&canonical_range),
            "the persisted record must verify against the rows retention reads back"
        );
    }

    #[test]
    fn rehashing_a_range_with_no_canonical_rows_is_an_error_not_an_empty_hash() {
        let store = rightkit_agent_engine_storage::Storage::open_in_memory().unwrap();
        let carried =
            rightkit_agent_engine_core::compaction_archive::CompactionProjectionRecord::build(
                "sess-missing",
                0,
                1,
                &[],
                String::new(),
            );
        // An empty range must not silently produce a hash-of-nothing that a
        // later verify would happily match, authorizing deletion of rows this
        // record never actually covered.
        assert!(rehash_over_canonical_rows(&store, &carried).is_err());
    }

    #[tokio::test]
    async fn failed_projection_rehash_leaves_archive_events_and_epoch_unchanged() {
        let storage =
            SharedStorage::new(rightkit_agent_engine_storage::Storage::open_in_memory().unwrap());
        storage
            .lock()
            .await
            .create_session(&rightkit_agent_engine_storage::SessionRow {
                id: "projection-failure".into(),
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
        let before_epoch = storage
            .lock()
            .await
            .compaction_epoch("projection-failure")
            .unwrap();
        let projection =
            rightkit_agent_engine_core::compaction_archive::CompactionProjectionRecord::build(
                "projection-failure",
                0,
                1,
                &[],
                "summary",
            );
        let result = SqliteCompactionArchiveSink::new(storage.clone())
            .archive(CompactionArchiveInput {
                session_id: "projection-failure".into(),
                created_at: "t".into(),
                messages: vec![],
                projection: Some(projection),
                lineage: None,
                snapshot: None,
                federation_decision: None,
                federation_request_started_at: None,
            })
            .await;
        let error = result.unwrap_err();
        assert_eq!(
            error.class,
            rightkit_agent_engine_core::CompactionFailureClassV1::StorageUnavailable
        );
        assert!(error.message.contains("projection not re-hashed"));
        let store = storage.lock().await;
        assert!(store
            .list_compaction_archives("projection-failure")
            .unwrap()
            .is_empty());
        assert!(store.all_events("projection-failure").unwrap().is_empty());
        assert_eq!(
            store.compaction_epoch("projection-failure").unwrap(),
            before_epoch
        );
    }

    /// Negative control (Z24 / CRA-11 residual): lineage must commit inside
    /// the SAME transaction as the archive row and epoch advance, never
    /// independently. Before this fix, `persist_lineage` was a second,
    /// independent best-effort write behind its own `self.storage.lock()`
    /// call, so a failed archive commit could not roll a persisted lineage
    /// event back -- and, symmetrically, nothing stopped a future
    /// regression from resurrecting that second path.
    ///
    /// Fault: drive `archive()` with a session id that does not exist, so
    /// `commit_compaction`'s single transaction fails and rolls back --
    /// while `input.lineage` is populated. If lineage were ever committed
    /// through a channel independent of that transaction (the old
    /// `persist_lineage` shape, or any reintroduced equivalent), the
    /// `LINEAGE_EVENT_KIND` event below would exist despite the archive
    /// failing. It must not: the archive row, the epoch, and the lineage
    /// event are all-or-nothing.
    #[tokio::test]
    async fn lineage_never_commits_independently_of_a_failed_archive() {
        use rightkit_agent_engine_core::compaction_archive::{
            CompactionLineageSegment, CompactionRolloutLineage, LINEAGE_EVENT_KIND,
        };

        let storage =
            SharedStorage::new(rightkit_agent_engine_storage::Storage::open_in_memory().unwrap());
        let sink = SqliteCompactionArchiveSink::new(storage.clone());

        let lineage = CompactionRolloutLineage {
            segments: vec![CompactionLineageSegment {
                session_id: "sess-no-such-session".into(),
                canonical_range: vec![rightkit_agent_engine_protocol::TranscriptMessage::User {
                    id: "m1".into(),
                    created_at: "t".into(),
                    content: "dropped by compaction".into(),
                    trust: None,
                    pinned: false,
                    attachments: Vec::new(),
                }],
                start_ordinal: 1,
                end: None,
                byte_length: 0,
            }],
        };

        let result = sink
            .archive(CompactionArchiveInput {
                session_id: "sess-no-such-session".into(),
                created_at: "t".into(),
                messages: vec![],
                projection: None,
                lineage: Some(lineage),
                snapshot: None,
                federation_decision: None,
                federation_request_started_at: None,
            })
            .await;
        assert!(
            result.is_err(),
            "archiving against an unknown session must fail closed"
        );

        let store = storage.lock().await;
        assert!(
            store
                .list_compaction_archives("sess-no-such-session")
                .unwrap()
                .is_empty(),
            "a failed commit must not leave a partial archive row"
        );
        assert!(
            store
                .all_events("sess-no-such-session")
                .unwrap()
                .into_iter()
                .all(|row| row.kind != LINEAGE_EVENT_KIND),
            "lineage must never persist when the archive transaction it rides inside rolls back"
        );
    }

    #[test]
    fn openapi_metadata_is_content_free() {
        let value = serde_json::to_string(&compaction_archive_openapi_components()).unwrap();
        assert!(value.contains("sha256"));
        assert!(!value.contains("messages"));
        assert!(!value.contains("transcriptJson"));
    }

    #[tokio::test]
    async fn archive_retrieval_requires_auth_and_returns_exact_messages() {
        const TOKEN: &str = "archive-test-token";
        let storage =
            SharedStorage::new(rightkit_agent_engine_storage::Storage::open_in_memory().unwrap());
        let messages = vec![rightkit_agent_engine_protocol::TranscriptMessage::User {
            id: "user-1".into(),
            created_at: "2026-07-17T00:00:00Z".into(),
            content: "recover this exactly".into(),
            trust: None,
            pinned: false,
            attachments: Vec::new(),
        }];
        let bytes = serde_json::to_vec(&messages).unwrap();
        storage
            .lock()
            .await
            .insert_compaction_archive(
                "archive-1",
                "session-1",
                "2026-07-17T00:00:00Z",
                &bytes,
                messages.len(),
            )
            .unwrap();
        let app = crate::api::build_router(DaemonState::new(TOKEN).with_storage(storage));
        let uri = "/sessions/session-1/compaction-archives/archive-1";

        let unauthenticated = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("authorization", format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["messages"][0]["content"], "recover this exactly");
        assert_eq!(value["sha256"].as_str().unwrap().len(), 64);
    }
}
