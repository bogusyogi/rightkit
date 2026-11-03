//! the host app's schema list for the session store's SQLite database.
//!
//! the host app owns the schemas (the `*_SQL` constants and `all_migrations`);
//! the runner is `rightkit_db::migrate`: one `BEGIN IMMEDIATE` transaction
//! per step together with its `PRAGMA user_version` bump, an online
//! pre-migration backup of a non-empty database, a `<db>.migrating` crash
//! marker, a forward-compatibility probe for newer additive databases, and
//! `rightkit_db::rollback_to` for reversible steps. [`migrate`] is the thin
//! adapter that supplies the list, the probe and the error mapping, then
//! re-runs the host app's additive column pass.
//!
//! Migration 1 is the baseline: it mirrors the original `schema.rs`
//! `CREATE TABLE IF NOT EXISTS` + additive-column set, so existing
//! (unversioned, `user_version = 0`) databases adopt versioning with zero
//! behavior change.

#![allow(clippy::doc_lazy_continuation)]
use crate::error::{Result, StorageError};
use rightkit_db::rusqlite;
use rightkit_db::rusqlite::Connection;
use rightkit_db::DbOptions;
use std::path::Path;

pub mod journal;

pub use journal::{read_pending_journal, PendingMigrationJournal};

/// One migration: a monotonically increasing version, a short name for
/// logging, and the SQL to run to get a database from `version - 1` to
/// `version`. Only additive, forward-only changes belong here — once a
/// migration has shipped, its SQL must never change (add a new migration
/// instead).
struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
    /// SQL that reverses this migration's effect, restoring the schema to
    /// exactly `version - 1`. `None` for every migration shipped before the
    /// rollback mechanism existed -- those remain forward-only until a
    /// down-migration is authored for each; `rightkit_db::rollback_to`
    /// refuses to cross a `None` boundary rather than guessing.
    down_sql: Option<&'static str>,
}

/// The baseline migration: today's schema, expressed as `CREATE TABLE IF NOT
/// EXISTS` so it is a no-op against a database that already has these tables
/// from the pre-versioning `schema.rs` path, and fully builds the schema on a
/// fresh database.
const BASELINE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    id                TEXT PRIMARY KEY,
    title             TEXT,
    workspace_root    TEXT,
    created_at        TEXT,
    updated_at        TEXT,
    parent_session_id TEXT,
    role              TEXT,
    status            TEXT,
    config_json       TEXT
);

CREATE TABLE IF NOT EXISTS events (
    session_id   TEXT,
    seq          INTEGER,
    kind         TEXT,
    payload_json TEXT,
    created_at   TEXT,
    PRIMARY KEY (session_id, seq)
);

CREATE TABLE IF NOT EXISTS tasks (
    session_id TEXT,
    id         TEXT,
    node_json  TEXT,
    PRIMARY KEY (session_id, id)
);

CREATE TABLE IF NOT EXISTS artifacts (
    id         TEXT PRIMARY KEY,
    session_id TEXT,
    path       TEXT,
    trust_label TEXT NOT NULL DEFAULT 'unreviewed',
    row_json   TEXT
);

CREATE TABLE IF NOT EXISTS memory (
    id         TEXT PRIMARY KEY,
    tier       TEXT,
    content    TEXT,
    score      REAL,
    created_at TEXT,
    meta_json  TEXT
);

CREATE TABLE IF NOT EXISTS remote_channels (
    id              TEXT PRIMARY KEY,
    kind            TEXT NOT NULL,
    profile_id      TEXT NOT NULL,
    enabled         INTEGER NOT NULL DEFAULT 0,
    configured      INTEGER NOT NULL DEFAULT 0,
    toolset         TEXT NOT NULL,
    delivery_policy TEXT NOT NULL,
    trust           TEXT NOT NULL,
    state           TEXT NOT NULL,
    last_success_at TEXT,
    last_error_at   TEXT,
    last_error      TEXT,
    updated_at      TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS remote_bindings (
    id              TEXT PRIMARY KEY,
    channel_id      TEXT NOT NULL,
    target_key      TEXT NOT NULL,
    session_id      TEXT,
    target_label    TEXT,
    target_redacted TEXT,
    status          TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    UNIQUE(channel_id, target_key)
);

CREATE TABLE IF NOT EXISTS remote_deliveries (
    id              TEXT PRIMARY KEY,
    channel_id      TEXT NOT NULL,
    binding_id      TEXT,
    target_redacted TEXT NOT NULL,
    status          TEXT NOT NULL,
    delivery_kind   TEXT NOT NULL,
    payload_json    TEXT NOT NULL,
    error           TEXT,
    created_at      TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tool_invocations (
    id                  TEXT PRIMARY KEY,
    session_id          TEXT NOT NULL,
    turn_id             TEXT NOT NULL,
    tool_name           TEXT NOT NULL,
    schema_version      TEXT NOT NULL,
    args_hash           TEXT NOT NULL,
    args_redacted_json  TEXT NOT NULL,
    lifecycle_state     TEXT NOT NULL,
    policy_verdict_id   TEXT,
    approval_id         TEXT,
    process_id          TEXT,
    trust_label         TEXT NOT NULL DEFAULT 'unreviewed',
    started_at          TEXT,
    completed_at        TEXT,
    output_hash         TEXT,
    artifact_ids_json   TEXT NOT NULL DEFAULT '[]',
    error_class         TEXT,
    updated_at          TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tool_invocation_events (
    invocation_id         TEXT NOT NULL,
    session_id            TEXT NOT NULL,
    seq                   INTEGER NOT NULL,
    kind                  TEXT NOT NULL,
    created_at            TEXT NOT NULL,
    payload_redacted_json TEXT NOT NULL,
    PRIMARY KEY (invocation_id, seq, kind)
);

CREATE TABLE IF NOT EXISTS tool_invocation_cancellations (
    invocation_id TEXT PRIMARY KEY,
    session_id    TEXT NOT NULL,
    reason        TEXT,
    created_at    TEXT NOT NULL
);

-- Full-text index over event payloads for cross-session recall
-- (search past conversations). Populated as events are appended.
CREATE VIRTUAL TABLE IF NOT EXISTS events_fts USING fts5(
    session_id UNINDEXED,
    kind UNINDEXED,
    payload
);

CREATE INDEX IF NOT EXISTS idx_sessions_parent ON sessions(parent_session_id);
CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at);
CREATE INDEX IF NOT EXISTS idx_artifacts_trust ON artifacts(trust_label);
CREATE INDEX IF NOT EXISTS idx_remote_bindings_channel ON remote_bindings(channel_id);
CREATE INDEX IF NOT EXISTS idx_remote_bindings_session ON remote_bindings(session_id);
CREATE INDEX IF NOT EXISTS idx_remote_deliveries_channel ON remote_deliveries(channel_id, created_at);
CREATE INDEX IF NOT EXISTS idx_remote_deliveries_status ON remote_deliveries(status, created_at);
CREATE INDEX IF NOT EXISTS idx_tool_invocations_session ON tool_invocations(session_id, updated_at);
CREATE INDEX IF NOT EXISTS idx_tool_invocations_state ON tool_invocations(lifecycle_state, updated_at);
CREATE INDEX IF NOT EXISTS idx_tool_invocation_events_session ON tool_invocation_events(session_id, seq);
CREATE INDEX IF NOT EXISTS idx_tool_invocation_cancellations_session ON tool_invocation_cancellations(session_id, created_at);

CREATE TABLE IF NOT EXISTS benchmarks (
    id         TEXT PRIMARY KEY,
    role       TEXT,
    model      TEXT,
    suite      TEXT,
    repeats    INTEGER,
    successes  INTEGER,
    task_id    TEXT,
    passed     INTEGER,
    agent_output TEXT,
    verify_output TEXT,
    verify_error TEXT,
    created_at TEXT
);

CREATE TABLE IF NOT EXISTS goals (
    id                 TEXT PRIMARY KEY,
    session_id         TEXT NOT NULL,
    text               TEXT NOT NULL,
    status             TEXT NOT NULL DEFAULT 'active',
    budget_usd         REAL NOT NULL DEFAULT 0,
    budget_tokens      INTEGER NOT NULL DEFAULT 0,
    tokens_used        INTEGER NOT NULL DEFAULT 0,
    continuation_count INTEGER NOT NULL DEFAULT 0,
    no_progress_count  INTEGER NOT NULL DEFAULT 0,
    started_at         TEXT NOT NULL,
    closed_at          TEXT
);

"#;

const COMPACTION_ARCHIVES_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS compaction_archives (
    id              TEXT PRIMARY KEY,
    session_id      TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    sha256          TEXT NOT NULL,
    byte_length     INTEGER NOT NULL,
    message_count   INTEGER NOT NULL,
    transcript_json BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_compaction_archives_session
    ON compaction_archives(session_id, created_at);
"#;

/// Make `events_fts` storage-owned and atomic with `events`. Mirroring is
/// done by SQLite triggers on the `events` table (AFTER INSERT / AFTER
/// UPDATE / AFTER DELETE), so every write path — append, replay, direct
/// SQL delete, future bulk-replay tooling — keeps the search index in sync
/// with the event log in the same transaction. Without triggers, a
/// half-applied append or a manual `DELETE FROM events` would leave stale
/// search hits; with them, the only way for `events_fts` to drift is for
/// the database itself to be inconsistent. The triggers use the
/// `payload`-only column model already declared in the baseline schema so
/// they do not change the FTS5 virtual-table layout.
const EVENTS_FTS_TRIGGERS_SQL: &str = r#"
DROP TRIGGER IF EXISTS events_ai_fts;
DROP TRIGGER IF EXISTS events_au_fts;
DROP TRIGGER IF EXISTS events_ad_fts;

CREATE TRIGGER events_ai_fts
AFTER INSERT ON events
BEGIN
    INSERT INTO events_fts(rowid, session_id, kind, payload)
    VALUES (new.rowid, new.session_id, new.kind, new.payload_json);
END;

CREATE TRIGGER events_au_fts
AFTER UPDATE ON events
BEGIN
    UPDATE events_fts
       SET session_id = new.session_id,
           kind       = new.kind,
           payload    = new.payload_json
     WHERE rowid = old.rowid;
END;

CREATE TRIGGER events_ad_fts
AFTER DELETE ON events
BEGIN
    DELETE FROM events_fts WHERE rowid = old.rowid;
END;
"#;

/// Persisted per-run sub-agent budget state. The daemon writes
/// `cumulative_used` after every successful `RunBudgetLedger::mark` so a
/// daemon restart restores the cap correctly without re-counting already-
/// completed sub-agents. The session row is the natural primary key; we
/// keep a separate one-row-per-session table rather than add columns to
/// `sessions` because the budget is a runtime-only concern and a separate
/// table makes the persistence boundary explicit (no FK surprises on
/// session delete).
const RUN_BUDGET_STATE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS run_budget_state (
    session_id       TEXT PRIMARY KEY,
    cumulative_used  INTEGER NOT NULL DEFAULT 0,
    last_admit_at    TEXT NOT NULL DEFAULT '0'
);
CREATE INDEX IF NOT EXISTS idx_run_budget_state_session
    ON run_budget_state(session_id);
"#;

/// Durable state for vendor-driver sessions and their session-scoped capability
/// leases. Vendor event identity is unique within a binding so reconnect/replay
/// can claim an event transactionally before applying any side effect.
const DRIVER_HOST_STATE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS driver_bindings (
    id                  TEXT PRIMARY KEY,
    driver_kind         TEXT NOT NULL,
    session_id          TEXT NOT NULL,
    vendor_session_id   TEXT,
    vendor_thread_id    TEXT,
    vendor_turn_id      TEXT,
    binary_version      TEXT NOT NULL,
    schema_hash         TEXT NOT NULL,
    status              TEXT NOT NULL,
    last_event_key      TEXT,
    last_event_cursor   TEXT,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    UNIQUE(driver_kind, session_id)
);

CREATE TABLE IF NOT EXISTS driver_event_inbox (
    binding_id          TEXT NOT NULL,
    event_key           TEXT NOT NULL,
    event_cursor        TEXT,
    event_kind          TEXT NOT NULL,
    payload_json        TEXT NOT NULL,
    status              TEXT NOT NULL,
    received_at         TEXT NOT NULL,
    processed_at        TEXT,
    PRIMARY KEY(binding_id, event_key)
);

CREATE TABLE IF NOT EXISTS session_capability_leases (
    id                  TEXT PRIMARY KEY,
    token_hash          TEXT NOT NULL UNIQUE,
    nonce               TEXT NOT NULL UNIQUE,
    session_id          TEXT NOT NULL,
    canonical_workspace TEXT NOT NULL,
    tool_ceiling_json   TEXT NOT NULL,
    created_at          TEXT NOT NULL,
    expires_at          TEXT NOT NULL,
    revoked_at          TEXT
);

CREATE INDEX IF NOT EXISTS idx_driver_bindings_session
    ON driver_bindings(session_id, driver_kind);
CREATE INDEX IF NOT EXISTS idx_driver_event_inbox_status
    ON driver_event_inbox(binding_id, status, received_at);
CREATE INDEX IF NOT EXISTS idx_session_capability_leases_session
    ON session_capability_leases(session_id, expires_at);
"#;

// Capability leases created by the initial driver-host migration did not retain their exact
// binding identity. They are short-lived credentials, so invalidate them during the additive
// migration rather than guessing when both Codex and Claude bindings exist for one session.
const DRIVER_LEASE_BINDING_SQL: &str = r#"
ALTER TABLE session_capability_leases
    ADD COLUMN binding_id TEXT NOT NULL DEFAULT '';
DELETE FROM session_capability_leases;
"#;

const DRIVER_EVENT_RESULT_SQL: &str = r#"
ALTER TABLE driver_event_inbox
    ADD COLUMN result_json TEXT;
"#;

/// Durable storage for the autonomous workflow control plane (ADR-0021).
///
/// The journal is the source of truth for lifecycle: every state change is
/// a `JournalEvent` row, ordered by `seq` per `run_id`. The `node_states`
/// projection is a materialization rebuilt from the journal; it is allowed
/// to be deleted and re-derived at any time. Atomicity between journal
/// append and projection update is required to be expressed in code (see
/// `WorkflowJournalWriter::append_with_projection`); no SQLite triggers
/// exist because the writer is the only allowed appender.
///
/// Schema is additive and idempotent — upgrading an older database runs
/// this migration once; nothing is dropped, renamed, or backfilled because
/// the workflow tables are gated by `features.workflows_enabled` and a fresh
/// database has always been at this version.
const WORKFLOW_CONTROL_PLANE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS workflow_definitions (
    id                    TEXT PRIMARY KEY,
    name                  TEXT NOT NULL,
    source_kind           TEXT NOT NULL DEFAULT 'manual',
    created_at            TEXT NOT NULL,
    updated_at            TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_workflow_definitions_source
    ON workflow_definitions(source_kind, updated_at);

CREATE TABLE IF NOT EXISTS workflow_versions (
    id                     TEXT PRIMARY KEY,
    workflow_id            TEXT NOT NULL,
    version_number         INTEGER NOT NULL,
    content_hash           TEXT NOT NULL,
    definition_json        TEXT NOT NULL,
    compiled_plan_json     TEXT NOT NULL,
    publication_state      TEXT NOT NULL,
    created_at             TEXT NOT NULL,
    published_at           TEXT,
    created_by             TEXT NOT NULL DEFAULT 'system',
    notes                  TEXT NOT NULL DEFAULT '',
    UNIQUE(workflow_id, version_number)
);
CREATE INDEX IF NOT EXISTS idx_workflow_versions_state
    ON workflow_versions(publication_state, created_at);
CREATE INDEX IF NOT EXISTS idx_workflow_versions_workflow
    ON workflow_versions(workflow_id, version_number DESC);

CREATE TABLE IF NOT EXISTS workflow_runs (
    id                     TEXT PRIMARY KEY,
    workflow_id            TEXT NOT NULL,
    workflow_version_id    TEXT NOT NULL,
    parent_run_id          TEXT,
    lifecycle_phase        TEXT NOT NULL,
    lifecycle_revision     INTEGER NOT NULL DEFAULT 0,
    initiated_by_kind      TEXT NOT NULL,
    initiated_by_ref       TEXT,
    trigger_payload_json   TEXT,
    input_json             TEXT NOT NULL DEFAULT '{}',
    started_at             TEXT NOT NULL,
    terminal_at            TEXT,
    terminal_failure_class TEXT,
    runs_root_durable      INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_version
    ON workflow_runs(workflow_version_id, started_at);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_phase
    ON workflow_runs(lifecycle_phase, started_at);
CREATE INDEX IF NOT EXISTS idx_workflow_runs_parent
    ON workflow_runs(parent_run_id);

CREATE TABLE IF NOT EXISTS workflow_journal (
    run_id                TEXT NOT NULL,
    seq                   INTEGER NOT NULL,
    revision              INTEGER NOT NULL,
    event_kind            TEXT NOT NULL,
    node_id               TEXT,
    payload_json          TEXT NOT NULL,
    created_at            TEXT NOT NULL,
    PRIMARY KEY(run_id, seq)
);
CREATE INDEX IF NOT EXISTS idx_workflow_journal_kind
    ON workflow_journal(event_kind, created_at);

CREATE TABLE IF NOT EXISTS workflow_node_states (
    run_id                TEXT NOT NULL,
    node_id               TEXT NOT NULL,
    phase                 TEXT NOT NULL,
    attempt               INTEGER NOT NULL DEFAULT 0,
    lease_holder          TEXT,
    lease_expires_at      TEXT,
    last_reducer_revision INTEGER NOT NULL DEFAULT 0,
    last_event_seq        INTEGER NOT NULL DEFAULT 0,
    last_failure_class    TEXT,
    last_updated_at       TEXT NOT NULL,
    PRIMARY KEY(run_id, node_id)
);
CREATE INDEX IF NOT EXISTS idx_workflow_node_states_phase
    ON workflow_node_states(phase, last_updated_at);

CREATE TABLE IF NOT EXISTS workflow_run_lease (
    run_id                TEXT PRIMARY KEY,
    lease_holder          TEXT NOT NULL,
    lease_expires_at      TEXT NOT NULL,
    lease_fence           INTEGER NOT NULL DEFAULT 0,
    updated_at            TEXT NOT NULL
);
"#;

/// WP-06/WP-07 persistence wave for the autonomous workflow control plane.
///
/// Adds the durable tables the trigger-event / occurrence / wait / approval /
/// effect / outbox / attention domain modules read and write. Every table
/// uses `CREATE TABLE IF NOT EXISTS` so the migration is safe to apply on
/// any existing database (a v8/v9/v10 DB has none of these tables, so the
/// `IF NOT EXISTS` clauses never short-circuit; a fresh DB also picks the
/// tables up). Schema invariants mirrored from
/// `docs/pending/workflow-control-plane-support/reference-schema.sql`,
/// reconciled with the storage crate's existing non-`STRICT` style. Durable
/// hashes and run-reference invariants are enforced by additive guard
/// migrations below.
///
/// Migration ordering:
/// * Depends on v8 (workflow_runs / workflow_journal / workflow_node_states
///   / workflow_run_lease) so `run_id` foreign keys resolve. The trigger /
///   event / occurrence / wait / approval / effect / outbox / attention
///   tables all reference `workflow_runs(id)`. Node identity remains the
///   `(run_id,node_id)` projection key, so node columns stay `TEXT` rather
///   than referring to a second node registry.
///
/// The `dedupe_key` unique constraints are the core idempotency invariant:
/// trigger events are unique per `(source, dedupe_key)`, occurrences are
/// unique per `(trigger_id, dedupe_key)`, and effects are unique per
/// `(adapter, idempotency_key)`. These match the domain modules' contract
/// for replay-safe admission.
const WORKFLOW_WP06_WP07_PERSISTENCE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS workflow_triggers (
    id                   TEXT PRIMARY KEY,
    workflow_id          TEXT NOT NULL,
    kind                 TEXT NOT NULL,
    enabled              INTEGER NOT NULL DEFAULT 0,
    cron_expression      TEXT,
    timezone             TEXT,
    version_pin          TEXT,
    dedupe_key           TEXT,
    overlap_policy       TEXT NOT NULL DEFAULT 'forbid',
    misfire_policy       TEXT NOT NULL DEFAULT 'skip',
    concurrency_cap      INTEGER NOT NULL DEFAULT 1,
    next_fire_at_unix_ms INTEGER,
    revision             INTEGER NOT NULL DEFAULT 0,
    created_at           TEXT NOT NULL,
    updated_at           TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_workflow_triggers_workflow_enabled
    ON workflow_triggers(workflow_id, enabled);
CREATE INDEX IF NOT EXISTS idx_workflow_triggers_next_fire
    ON workflow_triggers(enabled, next_fire_at_unix_ms);

CREATE TABLE IF NOT EXISTS workflow_trigger_events (
    id             TEXT PRIMARY KEY,
    trigger_id     TEXT,
    source         TEXT NOT NULL,
    event_type     TEXT NOT NULL,
    external_id    TEXT,
    dedupe_key     TEXT NOT NULL,
    content_hash   TEXT NOT NULL,
    trust_json     TEXT NOT NULL,
    data_json      TEXT NOT NULL,
    raw_artifact_id TEXT,
    occurred_at    TEXT NOT NULL,
    received_at    TEXT NOT NULL,
    UNIQUE(source, dedupe_key)
);
CREATE INDEX IF NOT EXISTS idx_workflow_trigger_events_trigger_received
    ON workflow_trigger_events(trigger_id, received_at);
CREATE INDEX IF NOT EXISTS idx_workflow_trigger_events_received
    ON workflow_trigger_events(received_at);

CREATE TABLE IF NOT EXISTS workflow_occurrences (
    id                  TEXT PRIMARY KEY,
    trigger_id          TEXT NOT NULL,
    trigger_event_id    TEXT,
    nominal_at_unix_ms  INTEGER NOT NULL,
    decision            TEXT NOT NULL,
    decision_reason     TEXT NOT NULL,
    selected_version_id TEXT,
    concurrency_key     TEXT,
    dedupe_key          TEXT NOT NULL,
    created_at          TEXT NOT NULL,
    UNIQUE(trigger_id, dedupe_key)
);
CREATE INDEX IF NOT EXISTS idx_workflow_occurrences_trigger_nominal
    ON workflow_occurrences(trigger_id, nominal_at_unix_ms);
CREATE INDEX IF NOT EXISTS idx_workflow_occurrences_concurrency
    ON workflow_occurrences(concurrency_key, decision);

CREATE TABLE IF NOT EXISTS workflow_waits (
    id              TEXT PRIMARY KEY,
    run_id          TEXT NOT NULL,
    node_id         TEXT NOT NULL,
    kind            TEXT NOT NULL,
    state           TEXT NOT NULL DEFAULT 'open',
    correlation_key TEXT NOT NULL DEFAULT '',
    topic           TEXT,
    filter_hash     TEXT,
    wake_at_unix_ms INTEGER,
    deadline_unix_ms INTEGER,
    matched_count   INTEGER NOT NULL DEFAULT 0,
    max_matches     INTEGER NOT NULL DEFAULT 1,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_workflow_waits_open_due
    ON workflow_waits(state, wake_at_unix_ms, deadline_unix_ms);
CREATE INDEX IF NOT EXISTS idx_workflow_waits_run_node
    ON workflow_waits(run_id, node_id);

CREATE TABLE IF NOT EXISTS workflow_approvals (
    id                     TEXT PRIMARY KEY,
    run_id                 TEXT NOT NULL,
    node_id                TEXT NOT NULL,
    version_id             TEXT NOT NULL,
    inputs_hash            TEXT NOT NULL,
    outputs_hash           TEXT NOT NULL,
    artifact_refs_json     TEXT NOT NULL DEFAULT '[]',
    capabilities_json      TEXT NOT NULL,
    effect_request_hash    TEXT NOT NULL,
    expires_at_unix_ms     INTEGER NOT NULL,
    role                   TEXT NOT NULL,
    one_shot               INTEGER NOT NULL DEFAULT 1,
    state                  TEXT NOT NULL DEFAULT 'requested',
    decision_id            TEXT,
    decision_actor         TEXT,
    decision_reason        TEXT,
    decision_signature     TEXT,
    decided_at_unix_ms     INTEGER,
    created_at             TEXT NOT NULL,
    updated_at             TEXT NOT NULL,
    UNIQUE(decision_id)
);
CREATE INDEX IF NOT EXISTS idx_workflow_approvals_open_expiry
    ON workflow_approvals(state, expires_at_unix_ms);
CREATE INDEX IF NOT EXISTS idx_workflow_approvals_run_node
    ON workflow_approvals(run_id, node_id);

CREATE TABLE IF NOT EXISTS workflow_effects (
    id                  TEXT PRIMARY KEY,
    run_id              TEXT NOT NULL,
    node_id             TEXT NOT NULL,
    adapter             TEXT NOT NULL,
    operation           TEXT NOT NULL,
    destination_hash    TEXT NOT NULL,
    payload_hash        TEXT NOT NULL,
    idempotency_key     TEXT NOT NULL,
    state               TEXT NOT NULL DEFAULT 'planned',
    attempt_count       INTEGER NOT NULL DEFAULT 0,
    max_attempts        INTEGER NOT NULL DEFAULT 1,
    receipt_json        TEXT,
    outcome_json        TEXT,
    reconciliation_json TEXT,
    failure_json        TEXT,
    requested_at_unix_ms INTEGER NOT NULL,
    ttl_unix_ms         INTEGER NOT NULL,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    UNIQUE(adapter, idempotency_key)
);
CREATE INDEX IF NOT EXISTS idx_workflow_effects_state_updated
    ON workflow_effects(state, updated_at);
CREATE INDEX IF NOT EXISTS idx_workflow_effects_run_node
    ON workflow_effects(run_id, node_id);

CREATE TABLE IF NOT EXISTS workflow_deliveries (
    id                TEXT PRIMARY KEY,
    run_id            TEXT NOT NULL,
    node_id           TEXT,
    effect_id         TEXT,
    delivery_type     TEXT NOT NULL,
    destination_json  TEXT NOT NULL,
    payload_hash      TEXT NOT NULL,
    state             TEXT NOT NULL DEFAULT 'pending',
    attempt_count     INTEGER NOT NULL DEFAULT 0,
    receipt_json      TEXT,
    last_error_json   TEXT,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_workflow_deliveries_run_state
    ON workflow_deliveries(run_id, state);
CREATE INDEX IF NOT EXISTS idx_workflow_deliveries_state_available
    ON workflow_deliveries(state, updated_at);

CREATE TABLE IF NOT EXISTS workflow_outbox (
    id               TEXT PRIMARY KEY,
    run_id           TEXT NOT NULL,
    kind             TEXT NOT NULL,
    topic            TEXT NOT NULL,
    dedupe_key       TEXT NOT NULL UNIQUE,
    payload_json     TEXT NOT NULL,
    state            TEXT NOT NULL DEFAULT 'pending',
    available_at     TEXT NOT NULL,
    attempt_count    INTEGER NOT NULL DEFAULT 0,
    max_attempts     INTEGER NOT NULL DEFAULT 5,
    last_error_json  TEXT,
    next_attempt_at  TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_workflow_outbox_state_available
    ON workflow_outbox(state, available_at);
CREATE INDEX IF NOT EXISTS idx_workflow_outbox_run_kind
    ON workflow_outbox(run_id, kind);

CREATE TABLE IF NOT EXISTS workflow_attention (
    id              TEXT PRIMARY KEY,
    run_id          TEXT,
    node_id         TEXT,
    effect_id       TEXT,
    approval_id     TEXT,
    kind            TEXT NOT NULL,
    state           TEXT NOT NULL DEFAULT 'open',
    severity        TEXT NOT NULL DEFAULT 'warning',
    title           TEXT NOT NULL,
    summary         TEXT NOT NULL,
    deep_link_json  TEXT NOT NULL DEFAULT '{}',
    dedupe_key      TEXT NOT NULL UNIQUE,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_workflow_attention_state_created
    ON workflow_attention(state, created_at);
CREATE INDEX IF NOT EXISTS idx_workflow_attention_run_kind_state
    ON workflow_attention(run_id, kind, state);

-- Terminal effect state cannot be silently mutated; mirrors the
-- reference-schema trigger so a reducer that accidentally retries a
-- completed effect fails loudly at the database boundary rather than
-- producing a second journal event for the same dispatch.
CREATE TRIGGER IF NOT EXISTS workflow_effect_terminal_guard
BEFORE UPDATE OF state ON workflow_effects
WHEN OLD.state IN ('confirmed','cancelled') AND NEW.state <> OLD.state
BEGIN
    SELECT RAISE(ABORT, 'terminal effect state cannot change');
END;

-- Approval one-shot invariant: a decision_id cannot be overwritten once
-- it has been recorded. Mirrors reference-schema `workflow_approval_decision_once`.
CREATE TRIGGER IF NOT EXISTS workflow_approval_decision_once
BEFORE UPDATE OF decision_id ON workflow_approvals
WHEN OLD.decision_id IS NOT NULL AND NEW.decision_id IS NOT OLD.decision_id
BEGIN
    SELECT RAISE(ABORT, 'approval decision is one-shot');
END;
"#;

const DRIVER_PROCESS_INSTANCE_SQL: &str = r#"
ALTER TABLE session_capability_leases
    ADD COLUMN process_instance_hash TEXT;
"#;

/// PR 5 of the multi-provider CLI runtime architecture (see
/// `docs/architecture/CLI-RUNTIME-SCOPE.md`,
/// PR 5). The `driver_bindings` row written by the sub-agent spawn
/// path now carries the parent agent id, so the UI's task graph can
/// draw the parent → child edge straight out of storage without
/// joining through the session row. The default `'' keeps every
/// pre-existing root-binding row valid: a root binding has no parent,
/// so `parent_agent_id = ''` is the unambiguous marker.
#[allow(dead_code)]
const DRIVER_BINDING_PARENT_AGENT_SQL: &str = r#"
ALTER TABLE driver_bindings
    ADD COLUMN parent_agent_id TEXT NOT NULL DEFAULT '';
CREATE INDEX IF NOT EXISTS idx_driver_bindings_parent_agent
    ON driver_bindings(parent_agent_id);
"#;

/// Generalize `driver_bindings` for the multi-provider CLI runtime and
/// add the `cli_account_profiles` and `cli_harness_attestations`
/// tables that PR 1 / PR 2 / PR 3 introduce.
///
/// Per the architecture plan (sections 19.1–19.3) this migration:
///
/// - keeps `driver_kind` during one compatibility release so older
///   binaries can still read the database; future migrations drop it
///   once forward/backward compatibility requirements are met;
/// - adds provider/transport/auth/attestation/executable/resume
///   columns to `driver_bindings`;
/// - adds the new `cli_account_profiles` table for vendor-CLI-managed
///   login locations;
/// - adds the new `cli_harness_attestations` table that PR 3 fills
///   in with live-binary attestation records;
/// - adds indexes that enforce one active binding per the host app
///   agent and accelerate provider+status queries.
///
/// No credential bytes are stored. the host app stores only non-secret
/// status and a one-way account fingerprint.
const CLI_RUNTIME_BINDING_SQL: &str = r#"
ALTER TABLE driver_bindings
    ADD COLUMN coderight_agent_id TEXT;
ALTER TABLE driver_bindings
    ADD COLUMN parent_agent_id TEXT;
ALTER TABLE driver_bindings
    ADD COLUMN provider_id TEXT;
ALTER TABLE driver_bindings
    ADD COLUMN transport_kind TEXT;
ALTER TABLE driver_bindings
    ADD COLUMN account_profile_id TEXT;
ALTER TABLE driver_bindings
    ADD COLUMN auth_mode TEXT;
ALTER TABLE driver_bindings
    ADD COLUMN account_fingerprint TEXT;
ALTER TABLE driver_bindings
    ADD COLUMN model_id TEXT;
ALTER TABLE driver_bindings
    ADD COLUMN executable_path TEXT;
ALTER TABLE driver_bindings
    ADD COLUMN executable_sha256 TEXT;
ALTER TABLE driver_bindings
    ADD COLUMN protocol_version TEXT;
ALTER TABLE driver_bindings
    ADD COLUMN profile_hash TEXT;
ALTER TABLE driver_bindings
    ADD COLUMN policy_hash TEXT;
ALTER TABLE driver_bindings
    ADD COLUMN tool_schema_hash TEXT;
ALTER TABLE driver_bindings
    ADD COLUMN harness_attestation_id TEXT;
ALTER TABLE driver_bindings
    ADD COLUMN isolation_mode TEXT;
ALTER TABLE driver_bindings
    ADD COLUMN resume_strategy TEXT;
ALTER TABLE driver_bindings
    ADD COLUMN context_epoch INTEGER NOT NULL DEFAULT 0;
ALTER TABLE driver_bindings
    ADD COLUMN process_generation INTEGER NOT NULL DEFAULT 0;

CREATE TABLE IF NOT EXISTS cli_account_profiles (
    id                   TEXT PRIMARY KEY,
    provider_id          TEXT NOT NULL,
    label                TEXT NOT NULL,
    managed_home         TEXT NOT NULL,
    auth_mode            TEXT NOT NULL,
    signed_in            INTEGER NOT NULL,
    account_fingerprint  TEXT,
    last_verified_at     TEXT,
    created_at           TEXT NOT NULL,
    updated_at           TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS cli_harness_attestations (
    id                   TEXT PRIMARY KEY,
    provider_id          TEXT NOT NULL,
    executable_sha256    TEXT NOT NULL,
    binary_version       TEXT NOT NULL,
    protocol_version     TEXT NOT NULL,
    profile_hash         TEXT NOT NULL,
    policy_hash          TEXT NOT NULL,
    tool_schema_hash     TEXT NOT NULL,
    operating_system     TEXT NOT NULL,
    architecture         TEXT NOT NULL,
    isolation_mode       TEXT NOT NULL,
    result               TEXT NOT NULL,
    evidence_json        TEXT NOT NULL,
    verified_at          TEXT NOT NULL,
    UNIQUE (
        provider_id,
        executable_sha256,
        profile_hash,
        policy_hash,
        tool_schema_hash,
        operating_system,
        architecture,
        isolation_mode
    )
);

CREATE INDEX IF NOT EXISTS idx_driver_bindings_agent
    ON driver_bindings(coderight_agent_id);
CREATE INDEX IF NOT EXISTS idx_driver_bindings_parent
    ON driver_bindings(parent_agent_id);
CREATE INDEX IF NOT EXISTS idx_driver_bindings_provider
    ON driver_bindings(provider_id, status);
CREATE UNIQUE INDEX IF NOT EXISTS idx_one_active_binding_per_agent
    ON driver_bindings(coderight_agent_id)
    WHERE status IN (
      'leased','starting','connected','running',
      'waiting_for_approval','waiting_for_child','resuming'
    );
"#;

const WORKFLOW_WP02_FOUNDATION_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS workflow_commands (run_id TEXT NOT NULL, command_id TEXT NOT NULL, expected_revision INTEGER NOT NULL, command_kind TEXT NOT NULL, payload_json TEXT NOT NULL, created_at TEXT NOT NULL, PRIMARY KEY(run_id, command_id));
CREATE TABLE IF NOT EXISTS workflow_queue (run_id TEXT NOT NULL, node_id TEXT NOT NULL, available_at TEXT NOT NULL, priority INTEGER NOT NULL DEFAULT 0, claimed_by TEXT, claim_fence INTEGER, PRIMARY KEY(run_id, node_id));
CREATE TABLE IF NOT EXISTS workflow_attempts (id TEXT PRIMARY KEY, run_id TEXT NOT NULL, node_id TEXT NOT NULL, attempt INTEGER NOT NULL, lease_fence INTEGER NOT NULL, state TEXT NOT NULL, started_at TEXT NOT NULL, finished_at TEXT);
CREATE TABLE IF NOT EXISTS workflow_claims (id TEXT PRIMARY KEY, run_id TEXT NOT NULL, node_id TEXT NOT NULL, attempt_id TEXT NOT NULL, holder TEXT NOT NULL, fence INTEGER NOT NULL, claimed_at TEXT NOT NULL, expires_at TEXT NOT NULL, UNIQUE(run_id, node_id, fence));
CREATE TABLE IF NOT EXISTS workflow_evidence (id TEXT PRIMARY KEY, run_id TEXT NOT NULL, node_id TEXT, kind TEXT NOT NULL, content_hash TEXT NOT NULL, artifact_ref TEXT, created_at TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_workflow_queue_ready ON workflow_queue(available_at, priority);
CREATE INDEX IF NOT EXISTS idx_workflow_claims_lease ON workflow_claims(expires_at);
"#;

// WP-03 extends the initial WP-02 attempt rows rather than replacing them:
// deployed databases may already contain queue work. Generation is the
// fencing token; every heartbeat or completion must match it.
const WORKFLOW_WP03_FENCING_SQL: &str = r#"
ALTER TABLE workflow_attempts ADD COLUMN heartbeat_at TEXT;
ALTER TABLE workflow_attempts ADD COLUMN result_json TEXT;
ALTER TABLE workflow_attempts ADD COLUMN failure_json TEXT;
ALTER TABLE workflow_attempts ADD COLUMN lease_holder TEXT;
ALTER TABLE workflow_attempts ADD COLUMN lease_expires_at TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS idx_workflow_attempt_generation
    ON workflow_attempts(run_id, node_id, lease_fence);
CREATE INDEX IF NOT EXISTS idx_workflow_attempt_live_lease
    ON workflow_attempts(state, lease_expires_at);
CREATE INDEX IF NOT EXISTS idx_workflow_queue_claim
    ON workflow_queue(claimed_by, available_at, priority DESC);
"#;

// v12 used `workflow_claims` for executor leases. That conflicts with the
// control-plane contract: claims are factual statements reviewed by evidence;
// leases belong to attempts/queue. Preserve old rows under an explicit name,
// then establish typed factual claim/evidence tables with real foreign keys.
const WORKFLOW_FACTUAL_CLAIMS_SQL: &str = r#"
ALTER TABLE workflow_claims RENAME TO workflow_attempt_claims;
ALTER TABLE workflow_evidence RENAME TO workflow_legacy_evidence;
CREATE TABLE workflow_claims (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
    node_id TEXT NOT NULL,
    claim_key TEXT NOT NULL,
    statement TEXT NOT NULL,
    state TEXT NOT NULL,
    statement_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(node_id, claim_key)
);
CREATE TABLE workflow_evidence (
    id TEXT PRIMARY KEY,
    claim_id TEXT NOT NULL REFERENCES workflow_claims(id) ON DELETE CASCADE,
    artifact_ref TEXT,
    evidence_type TEXT NOT NULL,
    source_json TEXT NOT NULL,
    verdict TEXT NOT NULL,
    evidence_hash TEXT NOT NULL,
    reviewer_attempt_id TEXT REFERENCES workflow_attempts(id) ON DELETE SET NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_workflow_claims_run_state ON workflow_claims(run_id, state);
CREATE INDEX idx_workflow_evidence_claim_verdict ON workflow_evidence(claim_id, verdict);
ALTER TABLE workflow_commands ADD COLUMN outcome_json TEXT NOT NULL DEFAULT '{}';
"#;

// SQLite cannot add foreign keys to existing v12 tables without a destructive
// table rebuild. These forward-only guards enforce equivalent admission-time
// invariants while retaining every deployed queue/attempt row.
const WORKFLOW_WP02_INVARIANTS_SQL: &str = r#"
CREATE TRIGGER IF NOT EXISTS workflow_command_run_guard
BEFORE INSERT ON workflow_commands
WHEN NOT EXISTS (SELECT 1 FROM workflow_runs WHERE id=NEW.run_id)
BEGIN SELECT RAISE(ABORT, 'workflow command references unknown run'); END;
CREATE TRIGGER IF NOT EXISTS workflow_queue_run_guard
BEFORE INSERT ON workflow_queue
WHEN NOT EXISTS (SELECT 1 FROM workflow_runs WHERE id=NEW.run_id)
BEGIN SELECT RAISE(ABORT, 'workflow queue references unknown run'); END;
CREATE TRIGGER IF NOT EXISTS workflow_attempt_run_guard
BEFORE INSERT ON workflow_attempts
WHEN NOT EXISTS (SELECT 1 FROM workflow_runs WHERE id=NEW.run_id)
BEGIN SELECT RAISE(ABORT, 'workflow attempt references unknown run'); END;
CREATE TRIGGER IF NOT EXISTS workflow_claim_hash_guard
BEFORE INSERT ON workflow_claims
WHEN NEW.statement_hash NOT GLOB 'sha256:*'
BEGIN SELECT RAISE(ABORT, 'workflow claim requires sha256 statement hash'); END;
CREATE TRIGGER IF NOT EXISTS workflow_evidence_hash_guard
BEFORE INSERT ON workflow_evidence
WHEN NEW.evidence_hash NOT GLOB 'sha256:*'
BEGIN SELECT RAISE(ABORT, 'workflow evidence requires sha256 evidence hash'); END;
"#;

/// The additive `ALTER TABLE ADD COLUMN` set baseline databases (whether
/// freshly created via `BASELINE_SQL` above, or pre-existing databases that
/// predate one or more of these columns) still need applied. `ALTER TABLE
/// ADD COLUMN` errors if the column already exists, so each statement here is
/// run best-effort and any "duplicate column" error is swallowed — mirrors
/// the historical behavior in `schema.rs` exactly.
const BASELINE_ADD_COLUMNS: &[&str] = &[
    "ALTER TABLE sessions ADD COLUMN updated_at TEXT",
    "ALTER TABLE sessions ADD COLUMN parent_session_id TEXT",
    "ALTER TABLE sessions ADD COLUMN role TEXT",
    "ALTER TABLE sessions ADD COLUMN status TEXT",
    "ALTER TABLE sessions ADD COLUMN tags TEXT DEFAULT ''",
    "ALTER TABLE sessions ADD COLUMN recovery_marker TEXT",
    "ALTER TABLE benchmarks ADD COLUMN task_id TEXT",
    "ALTER TABLE benchmarks ADD COLUMN passed INTEGER",
    "ALTER TABLE benchmarks ADD COLUMN agent_output TEXT",
    "ALTER TABLE benchmarks ADD COLUMN verify_output TEXT",
    "ALTER TABLE benchmarks ADD COLUMN verify_error TEXT",
];

const LEARNING_PROPOSALS_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS learning_proposals (
    id TEXT PRIMARY KEY,
    workflow_id TEXT NOT NULL,
    proposed_by TEXT NOT NULL,
    proposal_json TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    applied_at TEXT,
    rejection_reason TEXT
);
CREATE TABLE IF NOT EXISTS learning_eval_runs (
    id TEXT PRIMARY KEY,
    proposal_id TEXT NOT NULL,
    pass_rate_pct REAL NOT NULL,
    regression_count INTEGER NOT NULL,
    total_runs INTEGER NOT NULL,
    evidence_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_learning_proposals_status ON learning_proposals(status);
CREATE INDEX IF NOT EXISTS idx_learning_proposals_workflow_created ON learning_proposals(workflow_id, created_at);
CREATE INDEX IF NOT EXISTS idx_learning_eval_runs_proposal ON learning_eval_runs(proposal_id);
"#;

const WORKFLOW_INTEROP_LEARNING_RECEIPTS_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS workflow_import_quarantine (
    id TEXT PRIMARY KEY,
    raw_artifact_ref TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    signature_status TEXT NOT NULL,
    manifest_json TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'disabled',
    reviewer TEXT,
    reason TEXT NOT NULL,
    created_at TEXT NOT NULL,
    reviewed_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_workflow_import_quarantine_state ON workflow_import_quarantine(state, created_at DESC);
CREATE TABLE IF NOT EXISTS workflow_promotion_receipts (
    proposal_id TEXT PRIMARY KEY,
    approver TEXT NOT NULL,
    benchmark_id TEXT NOT NULL,
    activation_id TEXT NOT NULL,
    rollback_id TEXT NOT NULL,
    monitoring_id TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'active',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS workflow_monitoring_receipts (
    id TEXT PRIMARY KEY,
    proposal_id TEXT NOT NULL,
    monitoring_id TEXT NOT NULL,
    passed INTEGER NOT NULL,
    reason TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_workflow_monitoring_proposal ON workflow_monitoring_receipts(proposal_id, created_at DESC);
"#;

const WORKFLOW_TRIGGER_ADMISSION_RECEIPTS_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS workflow_trigger_raw_artifacts (
    id TEXT PRIMARY KEY,
    content_hash TEXT NOT NULL CHECK (content_hash GLOB 'sha256:*'),
    source TEXT NOT NULL,
    provenance_json TEXT NOT NULL,
    trust_json TEXT NOT NULL,
    storage_uri TEXT NOT NULL,
    metadata_json TEXT NOT NULL,
    byte_length INTEGER NOT NULL CHECK (byte_length >= 0),
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_workflow_trigger_raw_artifacts_hash
    ON workflow_trigger_raw_artifacts(content_hash);

CREATE TABLE IF NOT EXISTS workflow_trigger_admissions (
    trigger_event_id TEXT PRIMARY KEY
        REFERENCES workflow_trigger_events(id) ON DELETE CASCADE,
    occurrence_id TEXT NOT NULL UNIQUE
        REFERENCES workflow_occurrences(id) ON DELETE CASCADE,
    run_id TEXT UNIQUE
        REFERENCES workflow_runs(id) ON DELETE SET NULL,
    raw_artifact_id TEXT NOT NULL
        REFERENCES workflow_trigger_raw_artifacts(id),
    content_hash TEXT NOT NULL CHECK (content_hash GLOB 'sha256:*'),
    hmac_key_id TEXT NOT NULL,
    signature_hash TEXT NOT NULL CHECK (signature_hash GLOB 'sha256:*'),
    admitted_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_workflow_trigger_admissions_run
    ON workflow_trigger_admissions(run_id);

CREATE TRIGGER IF NOT EXISTS workflow_trigger_events_raw_artifact_insert_guard
BEFORE INSERT ON workflow_trigger_events
WHEN NEW.raw_artifact_id IS NOT NULL
 AND NOT EXISTS (
    SELECT 1 FROM workflow_trigger_raw_artifacts a
    WHERE a.id = NEW.raw_artifact_id
 )
BEGIN
    SELECT RAISE(ABORT, 'workflow trigger event raw artifact missing');
END;

CREATE TRIGGER IF NOT EXISTS workflow_trigger_events_raw_artifact_update_guard
BEFORE UPDATE OF raw_artifact_id ON workflow_trigger_events
WHEN NEW.raw_artifact_id IS NOT NULL
 AND NOT EXISTS (
    SELECT 1 FROM workflow_trigger_raw_artifacts a
    WHERE a.id = NEW.raw_artifact_id
 )
BEGIN
    SELECT RAISE(ABORT, 'workflow trigger event raw artifact missing');
END;
"#;

const WORKFLOW_EFFECT_CLAIM_FENCING_SQL: &str = r#"
ALTER TABLE workflow_effects ADD COLUMN claim_holder TEXT;
ALTER TABLE workflow_effects ADD COLUMN claim_fence INTEGER NOT NULL DEFAULT 0;
ALTER TABLE workflow_effects ADD COLUMN claim_expires_at_unix_ms INTEGER;
CREATE INDEX IF NOT EXISTS idx_workflow_effects_claim
    ON workflow_effects(state, claim_expires_at_unix_ms, claim_fence);
"#;

const WORKFLOW_OUTBOX_CLAIM_FENCING_SQL: &str = r#"
ALTER TABLE workflow_outbox ADD COLUMN claim_holder TEXT;
ALTER TABLE workflow_outbox ADD COLUMN claim_fence INTEGER NOT NULL DEFAULT 0;
ALTER TABLE workflow_outbox ADD COLUMN claim_expires_at TEXT;
CREATE INDEX IF NOT EXISTS idx_workflow_outbox_claim
    ON workflow_outbox(state, available_at, claim_expires_at, claim_fence);
"#;

const WORKFLOW_WAIT_COMPILED_FILTER_SQL: &str = r#"
ALTER TABLE workflow_waits ADD COLUMN filter_expression TEXT;
"#;

const GOAL_SNAPSHOT_V1_SQL: &str = r#"
ALTER TABLE goals ADD COLUMN dismissed_at TEXT;
ALTER TABLE goals ADD COLUMN decomposition_state TEXT NOT NULL DEFAULT 'missing';
ALTER TABLE goals ADD COLUMN plan_revision INTEGER NOT NULL DEFAULT 0;
ALTER TABLE goals ADD COLUMN now_text TEXT;
ALTER TABLE goals ADD COLUMN next_text TEXT;
ALTER TABLE goals ADD COLUMN plan_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE goals ADD COLUMN verification_state TEXT NOT NULL DEFAULT 'unverified';
"#;

// Generic inference authority identity. Credentials deliberately have no
// column: Direct secrets live solely in OS keychain entries.
const INFERENCE_AUTHORITY_V1_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS direct_inference_connections (
 id TEXT PRIMARY KEY, name TEXT NOT NULL, protocol TEXT NOT NULL CHECK(protocol IN ('openai_compatible','anthropic_compatible')),
 base_url TEXT NOT NULL, auth_kind TEXT NOT NULL, auth_header_name TEXT,
 public_headers_json TEXT NOT NULL, manual_models_json TEXT NOT NULL, trust_private_network INTEGER NOT NULL,
 credential_configured INTEGER NOT NULL, credential_revision INTEGER NOT NULL, configuration_revision INTEGER NOT NULL,
 created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS inference_route_attestations (
 id TEXT PRIMARY KEY, authority TEXT NOT NULL, connection_id TEXT NOT NULL, protocol TEXT NOT NULL, model_id TEXT NOT NULL,
 route_policy_id TEXT, selection_fingerprint TEXT NOT NULL UNIQUE, catalog_entry_hash TEXT NOT NULL,
 security_policy_hash TEXT NOT NULL, tool_schema_hash TEXT NOT NULL, configuration_revision INTEGER NOT NULL,
 credential_revision INTEGER NOT NULL, status TEXT NOT NULL, verified_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS inference_runtime_bindings (
 id TEXT PRIMARY KEY, session_id TEXT NOT NULL, agent_id TEXT NOT NULL, parent_agent_id TEXT,
 authority TEXT NOT NULL, connection_id TEXT NOT NULL, protocol TEXT NOT NULL, model_id TEXT NOT NULL,
 route_policy_id TEXT, selection_fingerprint TEXT NOT NULL, attestation_id TEXT NOT NULL, status TEXT NOT NULL,
 created_at TEXT NOT NULL, updated_at TEXT NOT NULL, UNIQUE(session_id, agent_id)
);
CREATE INDEX IF NOT EXISTS idx_inference_attestations_connection_status ON inference_route_attestations(connection_id, status);
CREATE INDEX IF NOT EXISTS idx_inference_bindings_session_status ON inference_runtime_bindings(session_id, status);
"#;

/// Durable machine-request idempotency. A claim is inserted before mutation;
/// the terminal response is filled only after the effect commits. Identical
/// retries therefore replay the original response, while a retry observed
/// after a crash remains explicitly `in_progress`/`unknown` instead of
/// blindly repeating a possibly-partial external effect.
const MACHINE_REQUEST_IDEMPOTENCY_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS machine_request_idempotency (
    session_id       TEXT NOT NULL,
    operation        TEXT NOT NULL,
    idempotency_key  TEXT NOT NULL,
    request_digest   TEXT NOT NULL,
    correlation_id   TEXT NOT NULL,
    effect_state     TEXT NOT NULL,
    response_json    TEXT,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    PRIMARY KEY (session_id, operation, idempotency_key)
);
CREATE INDEX IF NOT EXISTS idx_machine_request_correlation
    ON machine_request_idempotency(correlation_id);
"#;

/// PL-008: durable epoch-fenced controller handoff projection plus its
/// append-only transition journal. The projection is restart authority; the
/// journal preserves phase/cursor lineage for replay diagnosis.
const HANDOFF_STATE_V1_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS handoff_sessions (
    session_id          TEXT PRIMARY KEY,
    controller_epoch    INTEGER NOT NULL,
    source_target       TEXT NOT NULL,
    destination_target  TEXT NOT NULL,
    phase               TEXT NOT NULL,
    replay_cursor       INTEGER,
    terminal_detail     TEXT,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    CHECK (phase IN ('quiescing', 'source_quiesced', 'resumed', 'completed', 'cancelled', 'failed')),
    CHECK (source_target <> destination_target)
);
CREATE INDEX IF NOT EXISTS idx_handoff_sessions_phase
    ON handoff_sessions(phase, updated_at);

CREATE TABLE IF NOT EXISTS handoff_journal (
    session_id          TEXT NOT NULL,
    controller_epoch    INTEGER NOT NULL,
    sequence            INTEGER NOT NULL,
    phase               TEXT NOT NULL,
    replay_cursor       INTEGER,
    detail              TEXT,
    recorded_at         TEXT NOT NULL,
    PRIMARY KEY(session_id, sequence)
);
CREATE INDEX IF NOT EXISTS idx_handoff_journal_epoch
    ON handoff_journal(session_id, controller_epoch, sequence);

CREATE TABLE IF NOT EXISTS handoff_gap_messages (
    session_id       TEXT NOT NULL,
    controller_epoch INTEGER NOT NULL,
    message_key      TEXT NOT NULL,
    payload_json     TEXT NOT NULL,
    delivered_at     TEXT,
    created_at       TEXT NOT NULL,
    PRIMARY KEY(session_id, controller_epoch, message_key)
);
CREATE INDEX IF NOT EXISTS idx_handoff_gap_messages_pending
    ON handoff_gap_messages(session_id, controller_epoch, delivered_at);
"#;

const ADAPT_STORAGE_V1_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS adapt_guidance_records (
    guidance_id TEXT PRIMARY KEY,
    kind        TEXT NOT NULL,
    subject     TEXT NOT NULL,
    content     TEXT NOT NULL,
    digest      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_adapt_guidance_records_kind
    ON adapt_guidance_records(kind, guidance_id);

CREATE TABLE IF NOT EXISTS adapt_application_receipts (
    receipt_id  TEXT PRIMARY KEY,
    proposal_id TEXT NOT NULL,
    surface     TEXT NOT NULL,
    subject     TEXT NOT NULL,
    receipt_json TEXT NOT NULL,
    created_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_adapt_application_receipts_proposal
    ON adapt_application_receipts(proposal_id, created_at);
"#;

/// FM-175/FM-177/S:E11 — events quarantine table. Backs
/// `Storage::quarantine_malformed_payload_events` (glued-entry salvage) and
/// `Storage::checkpoint_events_after_gap` (corrupt-journal checkpoint):
/// rows are moved here, out of the live `events` table, when their
/// `payload_json` fails to parse or when they sit at/after the first
/// non-contiguous `seq` for a session — so replay keeps serving the
/// committed-good prefix instead of failing the whole session.
const EVENTS_QUARANTINE_V1_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS events_quarantine (
    session_id     TEXT NOT NULL,
    seq            INTEGER NOT NULL,
    kind           TEXT NOT NULL,
    payload_json   TEXT NOT NULL,
    created_at     TEXT NOT NULL,
    reason         TEXT NOT NULL,
    quarantined_at TEXT NOT NULL,
    PRIMARY KEY (session_id, seq)
);
CREATE INDEX IF NOT EXISTS idx_events_quarantine_session
    ON events_quarantine(session_id, seq);
"#;

const MACHINE_REQUEST_ADMISSION_SEQ_SQL: &str = r#"
ALTER TABLE machine_request_idempotency ADD COLUMN admission_seq INTEGER;
"#;

/// CRA-11 — durable, committed-boundary compaction epoch. Deliberately NOT
/// derived from live `Compact` transcript markers or `MAX(seq)` over
/// prunable `events` rows: this column is the sole source of truth, bumped
/// only inside `Storage::commit_compaction`'s single `IMMEDIATE` transaction
/// alongside the archive/projection/lineage/`SessionCompacted` event it
/// commits, so the epoch survives event pruning and strictly increases only
/// on a committed compaction boundary. A no-op, preparation error,
/// cancellation, failed archive, or failed commit never advances it.
const SESSION_COMPACTION_EPOCH_V1_SQL: &str = r#"
ALTER TABLE sessions ADD COLUMN compaction_epoch INTEGER NOT NULL DEFAULT 0;
"#;

/// Durable one-use fence for decision-bearing compactions. The key is owned
/// by the producer (canonical identity + restart epoch); the storage owner
/// consumes it in the same transaction as the archive commit.
const COMPACTION_DECISION_CONSUMPTIONS_V1_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS compaction_decision_consumptions (
    decision_identity TEXT NOT NULL,
    restart_epoch     INTEGER NOT NULL,
    archive_id        TEXT NOT NULL,
    consumed_at       TEXT NOT NULL,
    PRIMARY KEY (decision_identity, restart_epoch)
);
CREATE INDEX IF NOT EXISTS idx_compaction_decision_consumptions_archive
    ON compaction_decision_consumptions(archive_id);
"#;

/// Lane-C native session-transcript exporter cursor. One row per exported
/// session records the producer-side view of the absorbed stream:
/// `last_stream_seq` is the Membrane absorbed-stream position the next append
/// must extend (`expected_stream_seq`), and `last_source_seq` is the durable
/// `events.seq` already exported — the strict-tail replay cursor. Updates are
/// compare-and-set against the previously read pair so a second writer can
/// never silently rewind or fork the stream. `delete_session_tree` drops the
/// row with the session, and a `transcript_session_gone` (410) answer drops
/// it too — a tombstoned session never resurrects its cursor.
const MEMBRANE_TRANSCRIPT_CURSOR_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS membrane_transcript_cursors (
    session_id       TEXT PRIMARY KEY,
    last_stream_seq  INTEGER NOT NULL,
    last_source_seq  INTEGER NOT NULL,
    updated_at       TEXT NOT NULL
);
"#;

/// Independent host H4 observation sequence and durable-source replay cursor.
const ADAPT_OBSERVATION_CURSOR_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS adapt_observation_cursors (
    session_id           TEXT NOT NULL,
    task_id              TEXT NOT NULL,
    last_input_sequence  INTEGER NOT NULL,
    last_source_seq      INTEGER NOT NULL,
    provider             TEXT,
    model                TEXT,
    updated_at           TEXT NOT NULL,
    PRIMARY KEY (session_id, task_id)
);
"#;

/// Registered migrations, in ascending version order. `version` values must
/// be unique and contiguous starting at 1.
fn all_migrations() -> Vec<Migration> {
    vec![
        Migration {
            version: 1,
            name: "baseline schema",
            sql: BASELINE_SQL,
            down_sql: None,
        },
        Migration {
            version: 2,
            name: "durable compaction archives",
            sql: COMPACTION_ARCHIVES_SQL,
            down_sql: None,
        },
        Migration {
            version: 3,
            name: "trigger-owned events_fts mirror",
            sql: EVENTS_FTS_TRIGGERS_SQL,
            down_sql: None,
        },
        Migration {
            version: 4,
            name: "run-budget-state persistence",
            sql: RUN_BUDGET_STATE_SQL,
            down_sql: None,
        },
        Migration {
            version: 5,
            name: "driver-host session persistence",
            sql: DRIVER_HOST_STATE_SQL,
            down_sql: None,
        },
        Migration {
            version: 6,
            name: "bind capability leases to driver instances",
            sql: DRIVER_LEASE_BINDING_SQL,
            down_sql: None,
        },
        Migration {
            version: 7,
            name: "persist idempotent driver tool results",
            sql: DRIVER_EVENT_RESULT_SQL,
            down_sql: None,
        },
        Migration {
            version: 8,
            name: "bind leases to supervised driver process instances",
            sql: DRIVER_PROCESS_INSTANCE_SQL,
            down_sql: None,
        },
        Migration {
            version: 9,
            name: "generalize driver bindings for multi-provider CLI runtime",
            sql: CLI_RUNTIME_BINDING_SQL,
            down_sql: None,
        },
        Migration {
            version: 10,
            name: "autonomous workflow control plane storage",
            sql: WORKFLOW_CONTROL_PLANE_SQL,
            down_sql: None,
        },
        Migration {
            version: 11,
            name: "evaluation-gated learning proposals",
            sql: LEARNING_PROPOSALS_SQL,
            down_sql: None,
        },
        Migration {
            version: 12,
            name: "workflow WP-06/WP-07 trigger-event/occurrence/wait/approval/effect/outbox/attention tables",
            sql: WORKFLOW_WP06_WP07_PERSISTENCE_SQL,
            down_sql: None,
        },
        Migration {
            version: 13,
            name: "workflow WP-02 command queue claim evidence foundations",
            sql: WORKFLOW_WP02_FOUNDATION_SQL,
            down_sql: None,
        },
        Migration {
            version: 14,
            name: "workflow WP-03 attempt generation fencing",
            sql: WORKFLOW_WP03_FENCING_SQL,
            down_sql: None,
        },
        Migration {
            version: 15,
            name: "workflow factual claims evidence and command outcomes",
            sql: WORKFLOW_FACTUAL_CLAIMS_SQL,
            down_sql: None,
        },
        Migration {
            version: 16,
            name: "workflow WP-02 reference integrity guards",
            sql: WORKFLOW_WP02_INVARIANTS_SQL,
            down_sql: None,
        },
        Migration {
            version: 17,
            name: "workflow import quarantine and optimization receipts",
            sql: WORKFLOW_INTEROP_LEARNING_RECEIPTS_SQL,
            down_sql: None,
        },
        Migration {
            version: 18,
            name: "workflow unified trigger admission receipts",
            sql: WORKFLOW_TRIGGER_ADMISSION_RECEIPTS_SQL,
            down_sql: None,
        },
        Migration {
            version: 19,
            name: "workflow effect claim generation fencing",
            sql: WORKFLOW_EFFECT_CLAIM_FENCING_SQL,
            down_sql: None,
        },
        Migration {
            version: 20,
            name: "workflow outbox claim generation fencing",
            sql: WORKFLOW_OUTBOX_CLAIM_FENCING_SQL,
            down_sql: None,
        },
        Migration {
            version: 21,
            name: "workflow compiled event wait filters",
            sql: WORKFLOW_WAIT_COMPILED_FILTER_SQL,
            down_sql: None,
        },
        Migration {
            version: 22,
            name: "goal snapshot v1 persistence",
            sql: GOAL_SNAPSHOT_V1_SQL,
            down_sql: None,
        },
        Migration {
            version: 23,
            name: "inference authority v1",
            sql: INFERENCE_AUTHORITY_V1_SQL,
            down_sql: None,
        },
        Migration {
            version: 24,
            name: "digest-bound machine request idempotency",
            sql: MACHINE_REQUEST_IDEMPOTENCY_SQL,
            down_sql: None,
        },
        Migration {
            version: 25,
            name: "durable epoch-fenced handoff state",
            sql: HANDOFF_STATE_V1_SQL,
            down_sql: None,
        },
        Migration {
            version: 26,
            name: "durable Adapt guidance and application receipts",
            sql: ADAPT_STORAGE_V1_SQL,
            down_sql: None,
        },
        Migration {
            version: 27,
            name: "FM-175/FM-177 events quarantine table",
            sql: EVENTS_QUARANTINE_V1_SQL,
            down_sql: Some("DROP TABLE IF EXISTS events_quarantine;"),
        },
        Migration {
            version: 28,
            name: "durable user-message admission sequence",
            sql: MACHINE_REQUEST_ADMISSION_SEQ_SQL,
            down_sql: None,
        },
        Migration {
            version: 29,
            name: "CRA-11 durable compaction epoch",
            sql: SESSION_COMPACTION_EPOCH_V1_SQL,
            down_sql: None,
        },
        Migration {
            version: 30,
            name: "durable one-use compaction decision consumption",
            sql: COMPACTION_DECISION_CONSUMPTIONS_V1_SQL,
            down_sql: None,
        },
        Migration {
            version: 31,
            name: "native session transcript exporter cursor",
            sql: MEMBRANE_TRANSCRIPT_CURSOR_SQL,
            down_sql: Some("DROP TABLE IF EXISTS membrane_transcript_cursors;"),
        },
        Migration {
            version: 32,
            name: "durable H4 observation producer cursors",
            sql: ADAPT_OBSERVATION_CURSOR_SQL,
            down_sql: Some("DROP TABLE IF EXISTS adapt_observation_cursors;"),
        },
    ]
}

/// The migration list in RightKit's shape. the host app owns the schemas; the
/// runner (transactions, version bump, pre-migration backup, crash marker,
/// forward-compat probe, rollback) is `rightkit_db::migrate`.
fn rightkit_migrations() -> Vec<rightkit_db::Migration> {
    all_migrations()
        .into_iter()
        .map(|m| rightkit_db::Migration {
            version: m.version,
            name: m.name,
            up: if m.version == 3 {
                // Migration 3 installs the FTS mirror triggers; rows appended
                // before it ran are not mirrored, so it also backfills.
                rightkit_db::Step::Code(apply_events_fts_triggers_and_backfill)
            } else {
                rightkit_db::Step::Sql(m.sql)
            },
            down: m.down_sql,
        })
        .collect()
}

fn apply_events_fts_triggers_and_backfill(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(EVENTS_FTS_TRIGGERS_SQL)?;
    backfill_events_fts(conn);
    Ok(())
}

/// Best-effort additive-column pass, run at the end of every migrate call
/// (every statement is a no-op once applied).
fn apply_baseline_add_columns(conn: &Connection) {
    for stmt in BASELINE_ADD_COLUMNS {
        let _ = conn.execute(stmt, []);
    }
}

/// Bring `conn` (backed by the file at `db_path`, or `None` for `:memory:`)
/// up to date via `rightkit_db::migrate`, then run the host app's additive
/// column pass. Thin adapter: it only supplies the host app's migration list,
/// forward-compat probe and error mapping.
pub fn migrate(conn: &Connection, db_path: Option<&Path>) -> Result<()> {
    let options = rightkit_db::MigrateOptions {
        backup_options: db_path.map(|path| {
            DbOptions::new(path)
                .with_journal_env("RIGHTKIT_SQLITE_JOURNAL_MODE")
                .with_per_host_fallback(true)
        }),
        forward_compatible: Some(forward_schema_is_compatible),
        ..rightkit_db::MigrateOptions::default()
    };
    match rightkit_db::migrate(conn, db_path, &rightkit_migrations(), &options) {
        Ok(report) => {
            if let Some(marker) = &report.interrupted_previous {
                tracing::warn!(
                    from = marker.from,
                    to = marker.to,
                    pid = marker.pid,
                    "rightkit-agent-engine-storage: a previous migration batch did not finish"
                );
            }
            if report.forward_compatible {
                tracing::warn!(current = report.from, "opening forward-compatible database");
            }
            for version in &report.applied {
                tracing::info!(version, "rightkit-agent-engine-storage applied migration");
            }
            apply_baseline_add_columns(conn);
            Ok(())
        }
        Err(rightkit_db::DbError::NewerDatabase { current, max }) => {
            Err(StorageError::NewerDatabase { current, max })
        }
        Err(error) => Err(StorageError::from(error)),
    }
}

fn forward_schema_is_compatible(conn: &Connection) -> bool {
    [
        "SELECT id, title, workspace_root, created_at, updated_at, parent_session_id, role, status, config_json, tags, recovery_marker FROM sessions LIMIT 0",
        "SELECT session_id, seq, kind, payload_json, created_at FROM events LIMIT 0",
        "SELECT session_id, id, node_json FROM tasks LIMIT 0",
        "SELECT id, session_id, path, trust_label, row_json FROM artifacts LIMIT 0",
        "SELECT id, tier, content, score, created_at, meta_json FROM memory LIMIT 0",
        "SELECT id, session_id, turn_id, tool_name, schema_version, args_hash, args_redacted_json, lifecycle_state, policy_verdict_id, approval_id, process_id, trust_label, started_at, completed_at, output_hash, artifact_ids_json, error_class, updated_at FROM tool_invocations LIMIT 0",
        "SELECT invocation_id, session_id, seq, kind, created_at, payload_redacted_json FROM tool_invocation_events LIMIT 0",
        "SELECT invocation_id, session_id, reason, created_at FROM tool_invocation_cancellations LIMIT 0",
        "SELECT id, session_id, created_at, sha256, byte_length, message_count, transcript_json FROM compaction_archives LIMIT 0",
        "SELECT session_id, cumulative_used, last_admit_at FROM run_budget_state LIMIT 0",
    ]
    .iter()
    .all(|sql| conn.prepare(sql).is_ok())
}

/// Mirror every existing `events` row into `events_fts` via the events'
/// `rowid`. The triggers installed by migration 3 mirror future writes, but
/// rows appended before that migration ran (a database upgrading from v2)
/// need an explicit backfill. `INSERT OR IGNORE` on a UNINDEXED rowid
/// keeps the backfill safe to re-run.
fn backfill_events_fts(conn: &Connection) {
    let res = conn.execute(
        "INSERT OR IGNORE INTO events_fts(rowid, session_id, kind, payload) \
         SELECT rowid, session_id, kind, payload_json FROM events \
         WHERE rowid NOT IN (SELECT rowid FROM events_fts)",
        [],
    );
    if let Err(e) = res {
        tracing::warn!(
            error = %e,
            "events_fts backfill after trigger install encountered rows it could not mirror; \
             search index may miss events appended before migration v3. \
             Run `Storage::reindex_events_fts()` to rebuild from `events`."
        );
    }
}
