//! Row-mapping helpers shared by [`super::runs::WorkflowRepository`]'s
//! `query_map` closures. Pure conversion — no query text, no side effects.

use super::models::*;
use rightkit_db::rusqlite;

pub(crate) fn map_import_quarantine_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<WorkflowImportQuarantineRow> {
    Ok(WorkflowImportQuarantineRow {
        id: row.get(0)?,
        raw_artifact_ref: row.get(1)?,
        content_hash: row.get(2)?,
        signature_status: row.get(3)?,
        manifest_json: row.get(4)?,
        state: row.get(5)?,
        reviewer: row.get(6)?,
        reason: row.get(7)?,
        created_at: row.get(8)?,
        reviewed_at: row.get(9)?,
    })
}

pub(crate) fn map_wait_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowWaitRow> {
    Ok(WorkflowWaitRow {
        id: row.get(0)?,
        run_id: row.get(1)?,
        node_id: row.get(2)?,
        kind: row.get(3)?,
        state: row.get(4)?,
        correlation_key: row.get(5)?,
        topic: row.get(6)?,
        filter_expression: row.get(7)?,
        filter_hash: row.get(8)?,
        wake_at_unix_ms: row.get(9)?,
        deadline_unix_ms: row.get(10)?,
        matched_count: row.get(11)?,
        max_matches: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
    })
}

pub(crate) fn map_effect_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowEffectRow> {
    Ok(WorkflowEffectRow {
        id: row.get(0)?,
        run_id: row.get(1)?,
        node_id: row.get(2)?,
        adapter: row.get(3)?,
        operation: row.get(4)?,
        destination_hash: row.get(5)?,
        payload_hash: row.get(6)?,
        idempotency_key: row.get(7)?,
        state: row.get(8)?,
        attempt_count: row.get(9)?,
        max_attempts: row.get(10)?,
        receipt_json: row.get(11)?,
        outcome_json: row.get(12)?,
        reconciliation_json: row.get(13)?,
        failure_json: row.get(14)?,
        requested_at_unix_ms: row.get(15)?,
        ttl_unix_ms: row.get(16)?,
        created_at: row.get(17)?,
        updated_at: row.get(18)?,
    })
}

pub(crate) fn map_outbox_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowOutboxRow> {
    Ok(WorkflowOutboxRow {
        id: row.get(0)?,
        run_id: row.get(1)?,
        kind: row.get(2)?,
        topic: row.get(3)?,
        dedupe_key: row.get(4)?,
        payload_json: row.get(5)?,
        state: row.get(6)?,
        available_at: row.get(7)?,
        attempt_count: row.get(8)?,
        max_attempts: row.get(9)?,
        last_error_json: row.get(10)?,
        next_attempt_at: row.get(11)?,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
    })
}
