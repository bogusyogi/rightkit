//! THE migration gate: the only place on the open path that may run the
//! schema migration (`migrate` is private to `db` outside tests).
//!
//! Rules for a file-backed database (`gate_and_migrate`):
//! * Brand-new (stored version 0) or already current: nothing to decide.
//! * Stored version above this build's: refused for every channel, file untouched.
//! * Older, in the default (installed) folder, any channel: a verified backup
//!   first; no backup (not enough free disk included), no migration. A dev
//!   build is a full build installed in place, so it follows the release rule.
//! * Older, `dev` channel in a folder chosen explicitly (data-dir override):
//!   migrated without a backup. A release build backs up there too.
//! * RightKit's `.migrating` marker brackets every file-backed migration; one
//!   left by a crashed run is logged before the next attempt.
//! * Whenever a migration runs, `schema_migration_log` gets a row and the
//!   startup/diagnostics logs get a line.
//!
//! The channel and folder kind arrive as a [`GatePolicy`] so tests inject them
//! instead of weakening this function.
use super::data_dir::DirKind;
use super::migration_backup::{self, FreeBytes};
use super::migrations::{migrate, migrate_with, BackupPlan};
use super::{Connection, SCHEMA_VERSION};
use crate::host::{host, Channel};
use rightkit_db::rusqlite::params;
use rightkit_db::DbError;
use std::path::Path;

/// Created before any migration step; deliberately not a numbered schema step,
/// so it exists before stepping begins and needs no `SCHEMA_VERSION` bump.
const LOG_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS schema_migration_log (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    from_version  INTEGER NOT NULL,
    to_version    INTEGER NOT NULL,
    build_commit  TEXT NOT NULL,
    channel       TEXT NOT NULL,
    started_at    TEXT NOT NULL,
    finished_at   TEXT,
    outcome       TEXT NOT NULL
);";

/// Who is opening, and where.
#[derive(Clone)]
pub(crate) struct GatePolicy {
    pub channel: Channel,
    pub commit_short: String,
    pub dir_kind: DirKind,
    pub free_bytes: FreeBytes,
}

impl GatePolicy {
    /// This build's channel and commit with the given folder kind.
    pub(crate) fn for_this_build(dir_kind: DirKind) -> Self {
        let (channel, commit_short) = host().build();
        GatePolicy {
            channel,
            commit_short,
            dir_kind,
            free_bytes: migration_backup::platform_free_bytes,
        }
    }
}

fn now_stamp() -> String {
    crate::utc_stamp::rfc3339_secs()
}

/// One line in `startup.log` and `diagnostics.log` beside the database.
fn log(db_path: &Path, message: &str) {
    let Some(dir) = db_path.parent() else {
        return;
    };
    let line = format!("{} schema-migration {message}", now_stamp());
    for (file, max) in [("startup.log", 512_000), ("diagnostics.log", 2_000_000)] {
        let _ = host().append_rotating_log(&dir.join(file), max, &line);
    }
}

fn stored_version(conn: &Connection) -> Result<i64, String> {
    rightkit_db::user_version(conn).map_err(|e| format!("read user_version: {e}"))
}

fn downgrade_refusal(stored: i64) -> String {
    format!(
        "This {product} data was written by a newer version (data format {stored}; this \
         build understands up to {SCHEMA_VERSION}). Install the newer version. Your data \
         has not been changed.",
        product = host().product_name()
    )
}

fn short_outcome(err: &str) -> String {
    let flat: String = err.chars().filter(|c| !c.is_control()).take(300).collect();
    format!("failed: {flat}")
}

/// Run the migration behind the gate. See the module docs for the rules.
pub(crate) fn gate_and_migrate(
    conn: &Connection,
    db_path: &Path,
    key: &str,
    policy: &GatePolicy,
) -> Result<(), String> {
    let stored = stored_version(conn)?;
    let short = policy.commit_short.as_str();
    let channel = policy.channel.as_str();
    if super::migrations::stored_is_newer(conn)? {
        log(
            db_path,
            &format!(
                "refused downgrade stored={stored} supported={SCHEMA_VERSION} \
                 channel={channel} commit={short}"
            ),
        );
        return Err(downgrade_refusal(stored));
    }
    if stored == 0 || stored == SCHEMA_VERSION {
        // New database (created freely) or nothing to do.
        return migrate(conn);
    }

    // Only a dev build in a folder the developer named skips the backup.
    let (allowed_by, backup_first) = match (policy.channel, policy.dir_kind) {
        (Channel::Release, _) => ("release", true),
        (Channel::Dev, DirKind::Default) => ("dev-installed", true),
        (Channel::Dev, DirKind::Override) => ("override-folder", false),
    };

    // Free space first: a release build never migrates without room for the backup.
    if backup_first {
        if let Err(reason) =
            migration_backup::check_free_space_on(db_path, db_path, policy.free_bytes)
        {
            return Err(backup_refusal(db_path, stored, channel, short, &reason));
        }
    }
    let plan = if backup_first {
        let messages = migration_backup::message_count(conn)
            .map_err(|reason| backup_refusal(db_path, stored, channel, short, &reason))?;
        Some(BackupPlan {
            key,
            from: stored,
            messages,
        })
    } else {
        None
    };

    let started_at = now_stamp();
    let result = migrate_with(conn, Some(db_path), plan);
    let report = match result {
        // The crate backs up (and verifies) before the first step, so a failure there, whatever
        // its error kind (the backup copy, its check, its path, the pending marker), leaves the
        // data unchanged and writes no log row. Only a failed step (`MigrationFailed`) is logged
        // as a failed migration. The stored version is re-read so "unchanged" is never claimed
        // for a database that has moved.
        Err(e)
            if backup_first
                && !matches!(e, DbError::MigrationFailed { .. })
                && stored_version(conn) == Ok(stored) =>
        {
            return Err(backup_refusal(
                db_path,
                stored,
                channel,
                short,
                &e.to_string(),
            ));
        }
        Ok(report) => Ok(report),
        Err(e) => Err(e.to_string()),
    };
    let (backup, interrupted) = match &report {
        Ok(r) => (
            r.backup
                .as_ref()
                .and_then(|p| p.file_name())
                .map_or_else(|| "none".to_string(), |n| n.to_string_lossy().into_owned()),
            r.interrupted_previous.clone(),
        ),
        Err(_) => ("none".to_string(), rightkit_db::pending_marker(db_path)),
    };
    if let Some(marker) = interrupted {
        log(
            db_path,
            &format!(
                "previous migration did not finish from={} to={} pid={}",
                marker.from, marker.to, marker.pid
            ),
        );
    }
    log(
        db_path,
        &format!(
            "start from={stored} to={SCHEMA_VERSION} channel={channel} commit={short}              allowed={allowed_by} backup={backup}"
        ),
    );
    let outcome = match &report {
        Ok(_) => "ok".to_string(),
        Err(e) => short_outcome(e),
    };
    // The log row is written after the steps so a refused backup leaves the data unchanged.
    let _ = conn
        .execute_batch(LOG_TABLE_SQL)
        .map_err(|e| format!("create schema_migration_log: {e}"))
        .and_then(|()| {
            conn.execute(
                "INSERT INTO schema_migration_log
                    (from_version, to_version, build_commit, channel, started_at, finished_at, outcome)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    stored,
                    SCHEMA_VERSION,
                    short,
                    channel,
                    started_at,
                    now_stamp(),
                    outcome
                ],
            )
            .map_err(|e| format!("record migration: {e}"))
        });
    log(
        db_path,
        &format!("finish from={stored} to={SCHEMA_VERSION} commit={short} outcome={outcome}"),
    );
    report.map(|_| ())
}

fn backup_refusal(db_path: &Path, stored: i64, channel: &str, short: &str, reason: &str) -> String {
    log(
        db_path,
        &format!(
            "backup failed, not migrating from={stored} to={SCHEMA_VERSION}              channel={channel} commit={short} reason={reason}"
        ),
    );
    format!(
        "{} could not back up your data before upgrading it ({reason}).          Your data has not been changed.",
        host().product_name()
    )
}
