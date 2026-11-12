//! Daemon-side entry point for versioned state-store migrations: crash-
//! resume detection, an atomic-publish wrapper around
//! `rightkit_agent_engine_storage::Storage::open`, and a status/report surface a
//! `{app} migrate status` CLI verb (or an equivalent TUI/API route) can
//! be wired onto.
//!
//! `rightkit_agent_engine_storage::migrations` (see that crate for the full mechanism)
//! already gives per-migration transactional apply/rollback and a durable
//! pending-journal sidecar file written before a batch starts and cleared
//! only once every pending migration has committed. This module is the
//! daemon-owned caller of that mechanism: it is where CG-C01 ("versioned
//! daemon migrations journal, resume & atomically activate with explicit
//! compatibility & rollback boundaries") becomes something the daemon
//! actually runs at startup and reports on, rather than a library primitive
//! nothing calls.
//!
//! Donor lineage for the pieces assembled here (see
//! `docs/canon/daemon-master-canon-atoms.md` CG-C01 donor-verification
//! correction, 2026-08-29):
//! - codex `codex-rs/thread-store/src/local/rollout_migration.rs` — durable
//!   `.pending` journal, crash-resume, atomic publish (absorbed into
//!   `rightkit_agent_engine_storage::migrations::journal`, called from here).
//! - plandex `app/server/migrations` (golang-migrate up/down) — the
//!   down-migration convention (now `rightkit_db::rollback_to`, driven from
//!   `rightkit_agent_engine_storage::migrations`, with explicit fail-closed
//!   boundaries for legacy migrations that have no reversible definition).
//! - prime-agent two-phase update-restart fencing & jcode
//!   `jcode-build-support` binary activation — absorbed below as the durable
//!   `ActivationFence` install-layout seam; daemon reload owns process details.

use std::fs;
use std::path::{Path, PathBuf};

use rightkit_agent_engine_storage::migrations::{read_pending_journal, PendingMigrationJournal};
use rightkit_agent_engine_storage::{Storage, StorageError};

/// What happened when the daemon tried to bring its state store up to date
/// at startup. Meant to be logged as a structured event and surfaced
/// through `{app} migrate status` / the TUI startup banner — never a
/// silent side effect of `Storage::open`.
#[derive(Debug)]
pub struct StartupMigrationReport {
    /// Set if a previous run's migration batch did not clear its journal —
    /// i.e. the process crashed (or was killed) mid-migration. The current
    /// run's successful `Storage::open` below means `PRAGMA user_version`
    /// is already consistent (SQLite's per-migration transaction guarantees
    /// that); this field is purely "tell the operator their last shutdown
    /// was not clean," not a sign anything is currently broken.
    pub resumed_from_crash: Option<PendingMigrationJournal>,
    /// Whether this run's own migration batch found and applied pending
    /// migrations (vs. the database already being current).
    pub applied_migrations: bool,
}

/// Read-only migration state for an operator-selected daemon database.
#[derive(Debug)]
pub struct MigrationStatusReport {
    pub db_path: PathBuf,
    pub current_version: Option<i64>,
    pub pending_journal: Option<PendingMigrationJournal>,
}

/// Result of one staged activation: the replaced target and, when the target existed, the backup
/// of its prior bytes. The backup is a declared output of the activation transaction (it stays
/// after commit, unlike an implementation backup), so the caller can restore it until the
/// replacement has been judged healthy. Health, readiness and settlement stay with the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivationFence {
    pub request_id: String,
    pub staged_path: PathBuf,
    pub target_path: PathBuf,
    /// Where the prior target was kept; only exists when a prior target existed.
    pub backup_path: PathBuf,
}

fn file_name_of(path: &Path) -> Result<std::ffi::OsString, String> {
    path.file_name()
        .map(|n| n.to_os_string())
        .ok_or_else(|| format!("activation path has no file name: {}", path.display()))
}

/// The transaction root (the target's own directory) and the target's name within it.
fn activation_root(target: &Path) -> Result<(PathBuf, std::ffi::OsString), String> {
    let name = file_name_of(target)?;
    let root = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| "activation target has no parent".to_string())?;
    Ok((root.to_path_buf(), name))
}

fn backup_name(target_name: &std::ffi::OsStr, request_id: &str) -> std::ffi::OsString {
    let tag: String = request_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(48)
        .collect();
    let mut name = target_name.to_os_string();
    name.push(format!(".pre-activation-{}-{tag}.bak", std::process::id()));
    name
}

/// Publish staged install state through one `rightkit-fs` transaction. A prior target is copied
/// to the backup name (a create-new entry, same directory) and the staged bytes replace the
/// target (a replace entry); every candidate is staged before either is published, and a crash
/// before the commit decision leaves the prior target untouched.
pub fn activate_staged_path(
    staged_path: impl AsRef<Path>,
    target_path: impl AsRef<Path>,
    request_id: impl Into<String>,
) -> Result<ActivationFence, String> {
    let staged_path = staged_path.as_ref().to_path_buf();
    let target_path = target_path.as_ref().to_path_buf();
    if !staged_path.is_file() {
        return Err(format!(
            "staged activation is not a file: {}",
            staged_path.display()
        ));
    }
    let request_id = request_id.into();
    let (root, target_name) = activation_root(&target_path)?;
    let backup_file = backup_name(&target_name, &request_id);
    let backup_path = root.join(&backup_file);
    let mut entries = Vec::new();
    if target_path.exists() {
        entries.push(rightkit_fs::TransactionEntry::create_new(
            &backup_file,
            &target_path,
        ));
    }
    entries.push(rightkit_fs::TransactionEntry::new(
        &target_name,
        &staged_path,
    ));
    let report = rightkit_fs::FileTransaction::prepare(
        &root,
        entries,
        rightkit_fs::TransactionOptions::default(),
    )
    .and_then(|prepared| prepared.commit())
    .map_err(|e| format!("publish staged activation: {e}"))?;
    match report {
        rightkit_fs::TransactionReport::Committed(committed) => {
            for item in &committed.retained {
                tracing::warn!(path = %item.path.display(), reason = %item.reason, "activation leftover retained");
            }
        }
        rightkit_fs::TransactionReport::RolledBack(rolled) => {
            return Err(format!(
                "publish staged activation rolled back: {}",
                rolled.cause
            ));
        }
        rightkit_fs::TransactionReport::RecoveryRequired(required) => {
            return Err(format!(
                "publish staged activation needs recovery at next start: {}",
                required.cause
            ));
        }
        // `TransactionReport` is `#[non_exhaustive]`: an outcome a later owner version adds is
        // never read as success.
        other => {
            return Err(format!(
                "publish staged activation ended in an unrecognised state: {other:?}"
            ));
        }
    }
    Ok(ActivationFence {
        request_id,
        staged_path,
        target_path,
        backup_path,
    })
}

/// Restore the old target after a failed replacement health check: copy the backup back onto the
/// target through a transaction. When no prior target existed (no backup), the failed new target
/// is removed. Idempotent.
pub fn rollback_activation(fence: &ActivationFence) -> Result<(), String> {
    if !fence.backup_path.exists() {
        return match fs::remove_file(&fence.target_path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("remove failed activation: {e}")),
        };
    }
    let (root, target_name) = activation_root(&fence.target_path)?;
    let report = rightkit_fs::FileTransaction::prepare(
        &root,
        vec![rightkit_fs::TransactionEntry::new(
            &target_name,
            &fence.backup_path,
        )],
        rightkit_fs::TransactionOptions::default(),
    )
    .and_then(|prepared| prepared.commit())
    .map_err(|e| format!("restore activation backup: {e}"))?;
    match report {
        rightkit_fs::TransactionReport::Committed(_) => Ok(()),
        rightkit_fs::TransactionReport::RolledBack(rolled) => Err(format!(
            "restore activation backup rolled back: {}",
            rolled.cause
        )),
        rightkit_fs::TransactionReport::RecoveryRequired(required) => Err(format!(
            "restore activation backup needs recovery at next start: {}",
            required.cause
        )),
        // `#[non_exhaustive]`: an unknown outcome is a failure, never a restored target.
        other => Err(format!(
            "restore activation backup ended in an unrecognised state: {other:?}"
        )),
    }
}

/// Resolve any interrupted activation transaction in the target's directory. Startup calls this
/// before it reads activation or reload state: an uncommitted transaction is rolled back (the
/// prior target is untouched or restored), a committed one has its cleanup completed. Nothing is
/// created when no transaction exists.
pub fn recover_activation(
    target_path: impl AsRef<Path>,
) -> Result<rightkit_fs::RecoveryReport, String> {
    let (root, _) = activation_root(target_path.as_ref())?;
    rightkit_fs::recover_transactions(&root)
        .map_err(|e| format!("recover activation transactions in {}: {e}", root.display()))
}

/// Inspect migration state without opening the store, so `migrate status`
/// cannot apply a migration as a side effect.
pub fn migration_status<P: AsRef<Path>>(db_path: P) -> MigrationStatusReport {
    // Report the file the store actually opens (per-host fallback included), not the request.
    let db_path = effective_db_path(db_path.as_ref());
    MigrationStatusReport {
        current_version: current_user_version_best_effort(&db_path),
        pending_journal: read_pending_journal(&db_path),
        db_path,
    }
}

/// Apply pending migrations through the same daemon-owned open wrapper used at
/// startup. Keeping this explicit makes CLI apply observable without creating
/// a second migration implementation.
pub fn apply_migrations<P: AsRef<Path>>(
    db_path: P,
) -> Result<StartupMigrationReport, StorageError> {
    let (storage, report) = open_with_migration_report(db_path)?;
    drop(storage);
    Ok(report)
}

/// Open the daemon's SQLite state store at `db_path`, running any pending
/// migrations, with crash-resume detection layered on top.
///
/// This is a thin wrapper around `Storage::open` (which already calls
/// `rightkit_agent_engine_storage::migrations::migrate` internally, including the
/// journal write/clear around the batch — see that crate). What this adds:
///
/// 1. Before opening, check for a leftover pending-migration journal from a
///    previous run. Its presence means the last process to touch this
///    database crashed mid-migration; it does not mean the database is
///    corrupt (SQLite's transactional commit of `user_version` per
///    migration already guarantees that), but it is a crash-recovery
///    signal worth a startup log line and worth carrying into
///    `StartupMigrationReport` for the caller to surface.
/// 2. Record whether this run's own migration batch had pending migrations
///    to apply, from the `user_version` observed before vs. after.
pub fn open_with_migration_report<P: AsRef<Path>>(
    db_path: P,
) -> Result<(Storage, StartupMigrationReport), StorageError> {
    let db_path: PathBuf = db_path.as_ref().to_path_buf();

    // The file the storage layer will actually open: `Storage::open` may fall back to a per-host
    // sibling of the requested path, and its journal and versions live there, not at the request.
    let effective_before = effective_db_path(&db_path);
    let stale_journal = read_pending_journal(&effective_before);
    if let Some(journal) = &stale_journal {
        tracing::warn!(
            from_version = journal.from_version,
            to_version = journal.to_version,
            pending = ?journal.pending_versions,
            pid = journal.pid,
            "{} daemon: found a pending-migration journal from a previous run \
             that did not shut down cleanly; resuming automatically via PRAGMA user_version",
            crate::host_identity::app()
        );
    }

    let before_version = current_user_version_best_effort(&effective_before);
    let storage = Storage::open(&db_path)?;
    let after_version = current_user_version_best_effort(&effective_db_path(&db_path));

    let applied_migrations = match (before_version, after_version) {
        (Some(before), Some(after)) => after > before,
        // Fresh database (no file to probe before `Storage::open` created
        // it) always counts as "migrations ran" — a brand-new file starts
        // at version 0 and migration 1 always applies.
        (None, Some(_)) => true,
        _ => false,
    };

    Ok((
        storage,
        StartupMigrationReport {
            resumed_from_crash: stale_journal,
            applied_migrations,
        },
    ))
}

/// The database file `Storage::open(db_path)` opens: same options as the storage layer, resolved
/// through the owner's `effective_path`.
fn effective_db_path(db_path: &Path) -> PathBuf {
    rightkit_db::DbOptions::new(db_path)
        .with_journal_env("RIGHTKIT_SQLITE_JOURNAL_MODE")
        .with_per_host_fallback(true)
        .effective_path()
}

/// Best-effort read-only peek at `PRAGMA user_version` without going
/// through `Storage::open` (which would itself run migrations). Returns
/// `None` if the file does not exist yet or cannot be opened read-only —
/// both are normal for "database doesn't exist yet" and are handled by the
/// caller as "treat as version 0 / fresh."
fn current_user_version_best_effort(db_path: &Path) -> Option<i64> {
    if !db_path.exists() {
        return None;
    }
    let conn = rightkit_db::DbOptions::new(db_path).open_read_only().ok()?;
    conn.query_row("PRAGMA user_version", [], |r| r.get(0)).ok()
}
