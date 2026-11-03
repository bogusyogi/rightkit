use super::*;

use rightkit_db::rusqlite::Error as SqlError;
use rightkit_db::{
    add_column_if_missing, column_exists, migrate as apply_migrations, DbError, DbOptions,
    MigrateOptions, Migration, MigrationReport,
};

// A step error as the `rusqlite::Result` that `Migration::code` steps return.
fn text_err(message: String) -> SqlError {
    SqlError::ToSqlConversionFailure(message.into())
}

fn db_err(e: DbError) -> SqlError {
    match e {
        DbError::Sqlite(e) => e,
        other => text_err(other.to_string()),
    }
}

fn add_columns(conn: &Connection, columns: &[(&str, &str, &str)]) -> Result<(), SqlError> {
    for (table, column, decl) in columns {
        add_column_if_missing(conn, table, column, decl).map_err(db_err)?;
    }
    Ok(())
}

/// SQLite can't add a STORED generated column via ALTER TABLE (the column
/// would have to participate in row updates, and ALTER's COPY-then-DELETE
/// row format doesn't carry the expression). Rebuild the contacts_cache
/// table with the column baked in, preserving all rows. Mirrors the
/// `CREATE TABLE IF NOT EXISTS contacts_cache` definition above plus the
/// `name_lower` STORED column, and the two indexes that reference it.
fn recreate_contacts_cache_with_name_lower(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE contacts_cache__new (
             account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
             email      TEXT NOT NULL,
             name       TEXT NOT NULL DEFAULT '',
             photo_url  TEXT,
             name_lower TEXT GENERATED ALWAYS AS (lower(name)) STORED,
             PRIMARY KEY (account_id, email)
         );
         INSERT INTO contacts_cache__new(account_id, email, name, photo_url)
             SELECT account_id, email, name, photo_url FROM contacts_cache;
         DROP TABLE contacts_cache;
         ALTER TABLE contacts_cache__new RENAME TO contacts_cache;
         CREATE INDEX IF NOT EXISTS idx_contacts_name ON contacts_cache(name);
         CREATE INDEX IF NOT EXISTS idx_contacts_email ON contacts_cache(email);
         CREATE INDEX IF NOT EXISTS idx_contacts_name_lower ON contacts_cache(name_lower);",
    )
    .map_err(|e| format!("recreate contacts_cache with name_lower: {e}"))
}

/// Idempotent post-migration safety net. A DB that reached the current
/// `user_version` on an earlier build whose migration step didn't yet include a
/// table can be left missing it (e.g. `snoozes` — a v4 DB created before snoozes
/// was part of the v4 step), and `migrate` then skips it because the version is
/// already current. This re-asserts those objects on every open. It only uses
/// `IF NOT EXISTS` / conditional `ADD COLUMN`, so it never drops or rewrites data.
///
/// This is also the canonical home for the additive columns/tables that the
/// audit flagged as "not in a numbered SCHEMA_V*" — `contacts_cache`,
/// `messages.auth_results`, `labels.color`. They live here, applied
/// idempotently, *by design*: a raw versioned `ALTER TABLE ADD COLUMN` would
/// throw `duplicate column` on the many shipped DBs that already gained these on
/// a prior open via this very function.
pub(crate) fn ensure_runtime_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS snoozes (
           account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
           thread_id  TEXT NOT NULL,
           until_ms   INTEGER NOT NULL,
           PRIMARY KEY (account_id, thread_id)
         );
         CREATE INDEX IF NOT EXISTS idx_snoozes_until ON snoozes(until_ms);",
    )
    .map_err(|e| format!("ensure snoozes: {e}"))?;

    // Local contact cache for offline/instant compose autocomplete. Filled by a
    // background sync from the People API; queried locally first so typing in
    // To/Cc/Bcc works offline and without a per-keystroke network round-trip.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS contacts_cache (
           account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
           email      TEXT NOT NULL,
           name       TEXT NOT NULL DEFAULT '',
           photo_url  TEXT NOT NULL DEFAULT '',
           PRIMARY KEY (account_id, email)
         );
         CREATE INDEX IF NOT EXISTS idx_contacts_name ON contacts_cache(name);
         CREATE INDEX IF NOT EXISTS idx_contacts_email ON contacts_cache(email);",
    )
    .map_err(|e| format!("ensure contacts_cache: {e}"))?;

    // Additive columns — applied only when absent (ALTER TABLE ADD COLUMN has no
    // IF NOT EXISTS), so existing DBs gain them on open without a duplicate-column
    // error: messages.auth_results (raw
    // Authentication-Results SPF/DKIM/DMARC header), labels.color (user-label
    // background hex; server is source of truth, local picker writes back).
    add_columns(
        conn,
        &[
            ("messages", "auth_results", "TEXT"),
            ("labels", "color", "TEXT"),
        ],
    )
    .map_err(|e| format!("add runtime columns: {e}"))?;

    // L10: contacts_cache.name_lower — a STORED generated column so the
    // prefix-indexed contact search (db/outbox_contacts.rs::search_contacts_local)
    // can match name case-insensitively via a real index instead of
    // `lower(name) LIKE ?`, which SQLite cannot satisfy from a plain index on
    // `name`. `email` is already written lowercased by `cache_contacts`, so
    // only `name` needs the generated twin. Lives here (not a numbered
    // SCHEMA_V*) for the same reason as the other additive columns above:
    // ALTER TABLE ADD COLUMN has no IF NOT EXISTS, so a versioned migration
    // would throw duplicate-column on every DB that already has it.
    //
    // L15: SQLite refuses `ALTER TABLE ... ADD COLUMN ... GENERATED ALWAYS
    // AS ... STORED` (error: "cannot add a STORED column") — STORED columns
    // are part of the table's stored schema and can only be defined at
    // CREATE TABLE time. The fix is the standard "recreate the table"
    // dance: copy the rows into a fresh table with the new column baked in,
    // drop the old one, rename. `ensure_runtime_schema` already runs in a
    // single migration step, so the cost is one-time per DB. We detect the
    // error text to avoid rebuilding the table on already-upgraded DBs (the
    // check above would have caught the column).
    if !column_exists(conn, "contacts_cache", "name_lower")
        .map_err(|e| format!("inspect contacts_cache: {e}"))?
    {
        let alter = conn.execute(
            "ALTER TABLE contacts_cache ADD COLUMN name_lower TEXT
               GENERATED ALWAYS AS (lower(name)) STORED",
            [],
        );
        if let Err(e) = alter {
            let msg = e.to_string();
            if msg.contains("cannot add a STORED column") {
                recreate_contacts_cache_with_name_lower(conn)?;
            } else {
                return Err(format!("add contacts_cache.name_lower: {msg}"));
            }
        }
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_contacts_name_lower ON contacts_cache(name_lower);",
    )
    .map_err(|e| format!("ensure idx_contacts_name_lower: {e}"))?;

    // One-time heal for the unread-coverage fix. Older builds synced only the 50
    // newest INBOX messages and never the UNREAD label, so unread mail older than
    // that window was never mirrored — undercounting the unread badge and
    // emptying the Unread view. Now that UNREAD is a synced folder, clear each
    // account's history cursor ONCE so the next sync runs a full pass that pulls
    // unread mail of any age (read-only, additive upsert — no local data lost).
    // Guarded by a settings flag so it fires exactly once; new installs no-op
    // (no cursors yet, and the first sync is full regardless).
    const UNREAD_RESYNC_FLAG: &str = "heal_unread_resync_v1";
    if get_setting(conn, UNREAD_RESYNC_FLAG)?.is_none() {
        conn.execute("UPDATE sync_state SET last_history_id = NULL", [])
            .map_err(|e| format!("reset sync cursors: {e}"))?;
        set_setting(conn, UNREAD_RESYNC_FLAG, "1")?;
        // Notify the UI so it can show a one-time informational banner explaining
        // why a full resync is happening (RA-19). Cleared by the UI after display.
        set_setting(conn, "unread_resync_pending", "1")?;
    }
    Ok(())
}

// v2 adds thread_labels and fills it from the existing messages' label_ids.
fn v2_thread_labels(conn: &Connection) -> Result<(), SqlError> {
    conn.execute_batch(SCHEMA_V2)?;
    backfill_thread_labels(conn)
}

// v7 needs threads.important before any index can reference it.
fn v7_search_indexes(conn: &Connection) -> Result<(), SqlError> {
    add_columns(
        conn,
        &[("threads", "important", "INTEGER NOT NULL DEFAULT 0")],
    )?;
    conn.execute_batch(SCHEMA_V7)
}

// v11 adds message_recipients (see db_schema.rs SCHEMA_V11) and backfills it
// from the existing to_addrs/cc_addrs/bcc_addrs/from_addr columns in batches,
// inside this same step transaction: a crash mid-backfill rolls back to v10
// rather than leaving a half-populated normalized table (L10).
fn v11_message_recipients(conn: &Connection) -> Result<(), SqlError> {
    conn.execute_batch(SCHEMA_V11)?;
    crate::db::recipients::backfill_message_recipients(conn)
        .map(|_| ())
        .map_err(|e| text_err(format!("backfill message_recipients: {e}")))
}

// v15 moves full-text search to an external-content FTS5 table over extracted
// plain text (see db/fts_index.rs). Only DDL runs here; a populated mailbox is
// re-indexed by the resumable background job while the old index keeps serving.
fn v15_external_fts(conn: &Connection) -> Result<(), SqlError> {
    super::fts_index::migrate_v15(conn).map_err(text_err)
}

// v18 tags agent approvals and agent drafts with a kind (forward and delete
// reuse the send approval flow). Additive; every existing row keeps its meaning
// through the column default.
fn v18_agent_kinds(conn: &Connection) -> Result<(), SqlError> {
    add_columns(
        conn,
        &[
            (
                "agent_approvals",
                "kind",
                crate::db_schema::V18_APPROVAL_KIND,
            ),
            ("agent_drafts", "kind", crate::db_schema::V18_DRAFT_KIND),
        ],
    )
}

// v19 adds the decline-cooldown target key and the forward attachment digest.
// Additive nullable columns; no backfill.
fn v19_agent_cooldown(conn: &Connection) -> Result<(), SqlError> {
    add_columns(
        conn,
        &[
            (
                "agent_approvals",
                "target_key",
                crate::db_schema::V19_TARGET_KEY,
            ),
            (
                "agent_drafts",
                "attachment_digest",
                crate::db_schema::V19_ATTACHMENT_DIGEST,
            ),
        ],
    )
}

// v20 records which embedding pipeline produced each account's vectors.
// Additive nullable column; NULL forces one re-embed.
fn v20_pipeline_key(conn: &Connection) -> Result<(), SqlError> {
    add_columns(
        conn,
        &[(
            "semantic_index_state",
            "pipeline_key",
            crate::db_schema::V20_PIPELINE_KEY,
        )],
    )
}

// v22 moves the two durable queues onto rightkit-job-queue job tables
// (db/job_tables.rs): `outbox` (sends) and `pending_ops` (offline label
// mutations). Every legacy row is copied with its status, attempt count, due
// time, error and payload, the legacy tables are dropped, and the row counts
// are checked before the step's transaction commits. A step that fails rolls
// back to v21 with the legacy tables intact. A row is never re-queued or
// re-sent: queued stays queued, a sent row becomes complete, a row in any
// unrecognized state (`sending`) is kept in a state no drain picks up.
const OUTBOX_LEGACY: &str = "outbox_pre_v22";
const PENDING_OPS_LEGACY: &str = "pending_ops_pre_v22";

// ISO-8601 UTC text of an epoch-milliseconds column, the job table's own format.
fn iso_utc(ms: &str) -> String {
    format!("strftime('%Y-%m-%dT%H:%M:%SZ', ({ms}) / 1000, 'unixepoch')")
}

fn count_rows(conn: &Connection, table: &str) -> Result<i64, SqlError> {
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
}

fn is_legacy_queue(conn: &Connection, table: &str, legacy_column: &str) -> Result<bool, SqlError> {
    Ok(column_exists(conn, table, legacy_column).map_err(db_err)?
        && !column_exists(conn, table, "job_id").map_err(db_err)?)
}

fn v22_job_queue_tables(conn: &Connection) -> Result<(), SqlError> {
    let outbox_legacy = is_legacy_queue(conn, "outbox", "rfc822")?;
    let ops_legacy = is_legacy_queue(conn, "pending_ops", "op_class")?;
    if outbox_legacy {
        // v4 added send_at; a database that skipped it is healed on open, not here.
        add_columns(conn, &[("outbox", "send_at", "INTEGER")])?;
        conn.execute_batch(&format!("ALTER TABLE outbox RENAME TO {OUTBOX_LEGACY};"))?;
    }
    if ops_legacy {
        conn.execute_batch(&format!(
            "ALTER TABLE pending_ops RENAME TO {PENDING_OPS_LEGACY};"
        ))?;
    }
    super::job_tables::init_schemas(conn).map_err(|e| text_err(format!("job tables: {e}")))?;
    if outbox_legacy {
        conn.execute_batch(&format!(
            "INSERT INTO outbox (job_id, request_id, kind, request_json, status, attempts,
                                 error_json, created_at, updated_at, account_id, rfc822,
                                 draft_id, send_at, sent_at, created_ms)
             SELECT id, id, 'send', '{{}}',
                    CASE status WHEN 'queued' THEN 'queued' WHEN 'scheduled' THEN 'queued'
                                WHEN 'failed' THEN 'send_failed' WHEN 'sent' THEN 'complete'
                                ELSE 'sending' END,
                    attempts,
                    CASE WHEN last_error IS NULL THEN NULL
                         ELSE json_object('message', last_error) END,
                    {created}, {updated}, account_id, rfc822, draft_id,
                    CASE WHEN status = 'scheduled' AND send_at IS NULL THEN created_at
                         ELSE send_at END,
                    sent_at, created_at
             FROM {OUTBOX_LEGACY} ORDER BY rowid;
             UPDATE outbox SET next_attempt_at = COALESCE(send_at, created_ms)
              WHERE status <> 'complete';",
            created = iso_utc("created_at"),
            updated = iso_utc("COALESCE(sent_at, created_at)"),
        ))?;
        if count_rows(conn, OUTBOX_LEGACY)? != count_rows(conn, "outbox")? {
            return Err(text_err(
                "outbox rows were lost while moving to job tables".into(),
            ));
        }
        conn.execute_batch(&format!("DROP TABLE {OUTBOX_LEGACY};"))?;
    }
    if ops_legacy {
        conn.execute_batch(&format!(
            "INSERT INTO pending_ops (job_id, request_id, kind, request_json, status, attempts,
                                      created_at, updated_at, account_id, created_ms,
                                      next_attempt_at)
             SELECT id, id, op_class,
                    json_object('action', action, 'thread_id', thread_id,
                                'add_labels', json(CASE WHEN json_valid(add_labels)
                                                        THEN add_labels ELSE '[]' END),
                                'rem_labels', json(CASE WHEN json_valid(rem_labels)
                                                        THEN rem_labels ELSE '[]' END)),
                    'queued', attempts, {created}, {created}, account_id, created_at, created_at
             FROM {PENDING_OPS_LEGACY} ORDER BY rowid;",
            created = iso_utc("created_at"),
        ))?;
        if count_rows(conn, PENDING_OPS_LEGACY)? != count_rows(conn, "pending_ops")? {
            return Err(text_err(
                "pending_ops rows were lost while moving to job tables".into(),
            ));
        }
        conn.execute_batch(&format!("DROP TABLE {PENDING_OPS_LEGACY};"))?;
    }
    Ok(())
}

/// The schema history; version N is position N and the last is `SCHEMA_VERSION`.
/// Each step runs in its own transaction with its `user_version` bump, so a
/// failing step leaves the database at the last good version.
const MIGRATIONS: &[Migration] = &[
    Migration::sql(1, "base schema", SCHEMA_V1),
    Migration::code(2, "thread labels", v2_thread_labels),
    Migration::sql(3, "reply-to and unsubscribe headers", SCHEMA_V3),
    Migration::sql(4, "snoozes and scheduled send", SCHEMA_V4),
    Migration::sql(5, "pending ops queue", SCHEMA_V5),
    Migration::sql(6, "thread label scope index", SCHEMA_V6),
    Migration::code(7, "search indexes", v7_search_indexes),
    // v8 adds the partial index that powers the candidate-driven has:attachment path.
    Migration::sql(8, "has-attachment index", SCHEMA_V8),
    // v9 re-keys fts_messages to rowid = messages.rowid (one-time full FTS
    // rebuild; a no-op on fresh DBs).
    Migration::sql(9, "rowid-keyed fts", SCHEMA_V9),
    Migration::sql(10, "unread partial index", SCHEMA_V10),
    Migration::code(11, "message recipients", v11_message_recipients),
    Migration::sql(12, "size and message-id operators", SCHEMA_V12),
    Migration::sql(13, "semantic search storage", SCHEMA_V13),
    Migration::sql(14, "label-first thread index", SCHEMA_V14),
    Migration::code(15, "external-content fts", v15_external_fts),
    Migration::sql(16, "agent gate tables", crate::db_schema::SCHEMA_V16),
    Migration::sql(
        17,
        "agent drafts and approvals",
        crate::db_schema::SCHEMA_V17,
    ),
    Migration::code(18, "agent approval kinds", v18_agent_kinds),
    Migration::code(19, "agent decline cooldown", v19_agent_cooldown),
    Migration::code(20, "semantic pipeline key", v20_pipeline_key),
    Migration::sql(
        21,
        "agent attach approvals",
        crate::db_schema_v21::SCHEMA_V21,
    ),
    Migration::code(22, "job-queue outbox and pending ops", v22_job_queue_tables),
];

/// Whether a newer build wrote this database: the stored version is above the newest step.
pub(crate) fn stored_is_newer(conn: &Connection) -> Result<bool, String> {
    rightkit_db::stored_version_is_newer(
        conn,
        &MigrateOptions::default().version_storage,
        MIGRATIONS,
    )
    .map_err(|e| format!("read user_version: {e}"))
}

/// What the pre-migration backup check needs: the key to reopen the copy with,
/// the schema version the copy must carry and the source's message count.
pub(crate) struct BackupPlan<'a> {
    pub key: &'a str,
    pub from: i64,
    pub messages: Option<i64>,
}

struct VerifyCtx {
    key: String,
    from: i64,
    messages: Option<i64>,
    db_name: String,
}

thread_local! {
    // `MigrateOptions::verify_backup` and `backup_name` are bare fns, so the
    // per-run values travel in a thread-local set just before `migrate`.
    static VERIFY: std::cell::RefCell<Option<VerifyCtx>> = const { std::cell::RefCell::new(None) };
}

struct VerifyGuard;

impl Drop for VerifyGuard {
    fn drop(&mut self) {
        VERIFY.with(|v| *v.borrow_mut() = None);
    }
}

/// `<db file name>.pre-v<from>-<UTC stamp>.bak`; the crate prunes by the shared
/// `<db file name>.pre-` prefix.
fn backup_name(from: i64, _target: i64) -> String {
    let name = VERIFY.with(|v| v.borrow().as_ref().map(|c| c.db_name.clone()));
    let name = name.unwrap_or_else(|| super::db_file());
    let stamp = crate::utc_stamp::compact();
    format!("{name}.pre-v{from}-{stamp}.bak")
}

/// Reopen the backup with the key and compare it with the source. A backup
/// that fails is deleted so no unverified `.bak` is left behind.
fn verify_backup(path: &Path) -> Result<(), String> {
    let result = VERIFY.with(|v| match v.borrow().as_ref() {
        Some(c) => super::migration_backup::verify(path, &c.key, c.from, c.messages),
        None => Err("no backup check context".to_string()),
    });
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result
}

/// Migrate with no backup of ours (in-memory and brand-new databases, tests).
pub(crate) fn migrate(conn: &Connection) -> Result<(), String> {
    let db_path = conn.path().filter(|p| !p.is_empty()).map(Path::new);
    migrate_with(conn, db_path, None)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Migrate `conn`. With a `plan` the crate takes a keyed online backup first,
/// named `<db>.pre-v<from>-<stamp>.bak`, verifies it with [`BackupPlan`], keeps
/// the newest `KEEP_BACKUPS` and only then runs the steps; a backup failure
/// returns `DbError::Backup` with the database untouched. The newer-database
/// refusal lives in `migration_gate`, so no newer database counts as compatible.
pub(crate) fn migrate_with(
    conn: &Connection,
    db_path: Option<&Path>,
    plan: Option<BackupPlan<'_>>,
) -> Result<MigrationReport, DbError> {
    let mut options = MigrateOptions {
        backup: false,
        forward_compatible: None,
        // The best-effort `pragma_optimize` below runs instead, so a failed
        // optimize never fails a migration that already committed.
        optimize_after: false,
        ..MigrateOptions::default()
    };
    let _guard;
    if let (Some(plan), Some(path)) = (plan.as_ref(), db_path) {
        options.backup = true;
        options.backup_options = Some(DbOptions::new(path).with_passphrase(plan.key));
        options.backup_retention = Some(super::migration_backup::KEEP_BACKUPS);
        options.backup_name = Some(backup_name);
        options.verify_backup = Some(verify_backup);
        VERIFY.with(|v| {
            *v.borrow_mut() = Some(VerifyCtx {
                key: plan.key.to_string(),
                from: plan.from,
                messages: plan.messages,
                db_name: path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| super::db_file()),
            });
        });
        _guard = VerifyGuard;
    }
    let report = apply_migrations(conn, db_path, MIGRATIONS, &options)?;
    if !report.applied.is_empty() {
        // L9: refresh planner stats after the schema bump; new indexes change access patterns.
        crate::db::pragma_optimize(conn);
    }
    Ok(report)
}

/// Populate thread_labels from existing messages' label_ids JSON. Run by the
/// v1->v2 migration so upgrading a populated DB doesn't blank the label-scoped
/// folders. Idempotent (INSERT OR IGNORE).
fn backfill_thread_labels(conn: &Connection) -> Result<(), SqlError> {
    conn.execute(
        "INSERT OR IGNORE INTO thread_labels(account_id, thread_id, label)
         SELECT m.account_id, m.thread_id, je.value
         FROM messages m, json_each(m.label_ids) je
         WHERE m.label_ids IS NOT NULL",
        [],
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests;
