//! Ordered, transactional, crash-aware schema migrations.
//!
//! The library owns the *mechanism*; apps own the schemas. Rules, taken from the strongest
//! donors (CodeRight `storage::migrations`, MailRight `db::migrations`, WorkRight `local-store`):
//!
//! - versions are contiguous from 1 and default to `PRAGMA user_version`;
//! - each step runs in its own `BEGIN IMMEDIATE` transaction together with its version bump, so
//!   a failing step rolls back to the last good version;
//! - before the first pending step on a non-empty database an online backup is taken to
//!   `<db>.pre-v<target>.bak` (WAL-inclusive, atomic, owner-only);
//! - a `<db>.migrating` marker brackets the batch so a crash mid-batch is reported on next open;
//! - a database newer than this build is refused unless the app's probe says the subset it
//!   needs is still present (additive forward compatibility);
//! - rollback replays `down` steps and refuses when any step in the range lacks one.

use crate::recover::{
    allowed, is_unreadable, quarantine_database_journaled, unfinished_quarantine, RecoveryPolicy,
    QUARANTINE_JOURNAL_SUFFIX,
};
use crate::{DbError, DbOptions, Result};
use rusqlite::{Connection, ErrorCode, OptionalExtension};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub enum Step {
    Sql(&'static str),
    /// Rust step (data backfills, table rebuilds); runs inside the step's transaction.
    Code(fn(&Connection) -> rusqlite::Result<()>),
}

pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub up: Step,
    /// Reverse SQL; `None` makes this step unrevertable (rollback refuses to guess).
    pub down: Option<&'static str>,
}

impl Migration {
    pub const fn sql(version: i64, name: &'static str, up: &'static str) -> Self {
        Self {
            version,
            name,
            up: Step::Sql(up),
            down: None,
        }
    }

    pub const fn with_down(mut self, down: &'static str) -> Self {
        self.down = Some(down);
        self
    }

    pub const fn code(
        version: i64,
        name: &'static str,
        up: fn(&Connection) -> rusqlite::Result<()>,
    ) -> Self {
        Self {
            version,
            name,
            up: Step::Code(up),
            down: None,
        }
    }
}

/// Where migration versions are stored. Table storage uses Blueprint-compatible
/// `key TEXT PRIMARY KEY, value TEXT NOT NULL` rows, leaving `user_version` untouched.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum VersionStorage {
    #[default]
    UserVersion,
    /// Table names must contain only ASCII letters, digits, or underscores.
    /// Keys are bound parameters and may contain arbitrary text.
    Table { table: String, key: String },
    /// One row per applied version, shaped `"<version_column>" INTEGER PRIMARY KEY,
    /// "<applied_at_column>" INTEGER NOT NULL` (ViewRight's `_migrations` shape). The applied
    /// version is the highest stored `version_column`; a rollback deletes the top row. The table is
    /// created on the first write when absent. Identifiers must be ASCII letters, digits, or
    /// underscores. `applied_at` stores unix seconds.
    VersionTable {
        table: String,
        version_column: String,
        applied_at_column: String,
    },
}

impl VersionStorage {
    /// [`VersionStorage::VersionTable`] with the `version` and `applied_at` column names.
    pub fn version_table(table: impl Into<String>) -> Self {
        Self::VersionTable {
            table: table.into(),
            version_column: "version".into(),
            applied_at_column: "applied_at".into(),
        }
    }
}

/// An app's existing one-row-per-version table, read once to adopt its applied version (see
/// [`MigrateOptions::import_legacy_versions`]). Only the highest `version_column` value is used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyVersionTable {
    pub table: String,
    pub version_column: String,
}

/// Checks a pre-migration backup before the first step runs; `Err(reason)` stops the migration.
pub type VerifyBackup = fn(&Path) -> std::result::Result<(), String>;
#[derive(Clone)]
pub struct MigrateOptions {
    /// Version location shared by reads and transactional migration writes.
    pub version_storage: VersionStorage,
    /// Take a pre-migration backup when migrating an existing (version > 0) on-disk database.
    pub backup: bool,
    /// Key source for the backup (SQLCipher); `DbOptions::open_writer_migrated` fills this in.
    pub backup_options: Option<DbOptions>,
    /// For a database newer than this build: `true` means the subset this build reads/writes is
    /// intact, so open it read-write without migrating.
    pub forward_compatible: Option<fn(&Connection) -> bool>,
    /// Refresh planner statistics after migrating (`PRAGMA optimize`).
    pub optimize_after: bool,
    /// Adopt an existing app version table (see [`MigrateOptions::import_legacy_versions`]). Default none.
    pub legacy_versions: Option<LegacyVersionTable>,
    /// Name of the pre-migration backup, given the current and target versions. Default
    /// `<db>.pre-v<target>.bak` beside the database. Must be a plain file name that is not the
    /// database's own name (a name with a path separator or NUL is refused as
    /// [`DbError::Backup`]).
    pub backup_name: Option<fn(current: i64, target: i64) -> String>,
    /// Checks the pre-migration backup before the first step runs. `Err(reason)` stops the
    /// migration with [`DbError::Backup`]. The live database is untouched and the backup is kept.
    pub verify_backup: Option<VerifyBackup>,
    /// Keeps only the newest `n` pre-migration backups of this database, by modification time:
    /// after each backup, older files named `<db>.pre-*.bak` in the same directory are removed.
    /// The backup just taken is always kept. Removal is best effort and never fails the migration.
    /// Default `None`: every backup is kept.
    pub backup_retention: Option<usize>,
}

impl Default for MigrateOptions {
    fn default() -> Self {
        Self {
            version_storage: VersionStorage::default(),
            backup: true,
            backup_options: None,
            forward_compatible: None,
            optimize_after: true,
            legacy_versions: None,
            backup_name: None,
            verify_backup: None,
            backup_retention: None,
        }
    }
}

impl MigrateOptions {
    /// Adopt the applied version of an app's existing version table, once, so its migrations are
    /// not re-run. Applies only when the configured storage reads version 0 and the legacy table's
    /// highest version is above zero. The configured storage is then seeded to that version (a
    /// `VersionTable` gets one row per version up to it), and the migrations up to it are skipped.
    /// The legacy table is only read, never written. Idempotent: once seeded, the configured
    /// storage is above zero, so later opens do not consult the legacy table. Rolling back to
    /// version 0 while the legacy table still exists re-adopts it on the next open, so drop or
    /// rename that table after adoption. A legacy version above this build's newest migration is
    /// refused as [`DbError::NewerDatabase`] and nothing is written.
    pub fn import_legacy_versions(mut self, legacy: LegacyVersionTable) -> Self {
        self.legacy_versions = Some(legacy);
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MigrationReport {
    pub from: i64,
    pub to: i64,
    pub applied: Vec<i64>,
    pub backup: Option<PathBuf>,
    /// Opened a newer-but-compatible database without migrating.
    pub forward_compatible: bool,
    /// A previous batch started but never finished (crash or failed step).
    pub interrupted_previous: Option<PendingMarker>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingMarker {
    pub from: i64,
    pub to: i64,
    pub pid: u32,
}

pub fn user_version(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

fn set_user_version(conn: &Connection, version: i64) -> rusqlite::Result<()> {
    // `user_version` cannot be bound as a parameter; the value is an i64, never text.
    conn.execute_batch(&format!("PRAGMA user_version = {version};"))
}

/// Read the selected migration version without creating a table or row.
/// Missing table/key means zero; malformed or negative versions are refused.
pub fn migration_version(conn: &Connection, storage: &VersionStorage) -> Result<i64> {
    match storage {
        VersionStorage::UserVersion => user_version(conn),
        VersionStorage::Table { table, key } => key_value_version(conn, table, key),
        VersionStorage::VersionTable {
            table,
            version_column,
            ..
        } => version_row_max(conn, table, version_column),
    }
}

fn key_value_version(conn: &Connection, table: &str, key: &str) -> Result<i64> {
    check_ident(table)?;
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1 COLLATE NOCASE)",
        [table],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(0);
    }
    let value: Option<String> = conn
        .query_row(
            &format!("SELECT CAST(\"value\" AS TEXT) FROM \"{table}\" WHERE \"key\"=?1"),
            [key],
            |row| row.get(0),
        )
        .optional()?;
    match value {
        None => Ok(0),
        Some(value) => value
            .parse::<i64>()
            .ok()
            .filter(|version| *version >= 0)
            .ok_or_else(|| DbError::InvalidMigrations(format!("invalid stored version {value:?}"))),
    }
}

// Called only inside the migration/down transaction, after identifier validation.
fn set_key_value_version(
    conn: &Connection,
    storage: &VersionStorage,
    version: i64,
) -> rusqlite::Result<()> {
    match storage {
        VersionStorage::UserVersion => set_user_version(conn, version),
        VersionStorage::Table { table, key } => {
            conn.execute_batch(&format!(
                "CREATE TABLE IF NOT EXISTS \"{table}\" (\"key\" TEXT PRIMARY KEY, \"value\" TEXT NOT NULL);"
            ))?;
            conn.execute(
                &format!(
                    "INSERT INTO \"{table}\"(\"key\",\"value\") VALUES(?1,?2) \
                     ON CONFLICT(\"key\") DO UPDATE SET \"value\"=excluded.\"value\""
                ),
                rusqlite::params![key, version.to_string()],
            )?;
            Ok(())
        }
        // Routed to `advance_version` / `retreat_version`, never reached through them.
        VersionStorage::VersionTable { .. } => Err(rusqlite::Error::InvalidQuery),
    }
}

/// Make `version` the applied version: a forward step, inside its transaction.
fn advance_version(
    conn: &Connection,
    storage: &VersionStorage,
    version: i64,
) -> rusqlite::Result<()> {
    match storage {
        VersionStorage::VersionTable {
            table,
            version_column,
            applied_at_column,
        } => {
            conn.execute_batch(&format!(
                "CREATE TABLE IF NOT EXISTS \"{table}\" (\"{version_column}\" INTEGER PRIMARY KEY, \"{applied_at_column}\" INTEGER NOT NULL);"
            ))?;
            conn.execute(
                &format!("DELETE FROM \"{table}\" WHERE \"{version_column}\" = ?1"),
                [version],
            )?;
            conn.execute(
                &format!(
                    "INSERT INTO \"{table}\" (\"{version_column}\", \"{applied_at_column}\") VALUES (?1, ?2)"
                ),
                rusqlite::params![version, now_secs()],
            )?;
            Ok(())
        }
        _ => set_key_value_version(conn, storage, version),
    }
}

/// Un-apply `version`: a rollback step, so the applied version becomes `version - 1`.
fn retreat_version(
    conn: &Connection,
    storage: &VersionStorage,
    version: i64,
) -> rusqlite::Result<()> {
    match storage {
        VersionStorage::VersionTable {
            table,
            version_column,
            ..
        } => {
            conn.execute(
                &format!("DELETE FROM \"{table}\" WHERE \"{version_column}\" = ?1"),
                [version],
            )?;
            Ok(())
        }
        _ => set_key_value_version(conn, storage, version - 1),
    }
}

/// Seed a database at version 0 with `through` as its applied version (legacy adoption). A
/// `VersionTable` gets one row per version; the other storages take the single value.
fn adopt_legacy_version(conn: &Connection, storage: &VersionStorage, through: i64) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    let seeded = match storage {
        VersionStorage::VersionTable { .. } => {
            (1..=through).try_for_each(|version| advance_version(conn, storage, version))
        }
        _ => advance_version(conn, storage, through),
    };
    match seeded {
        Ok(()) => {
            conn.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK;");
            Err(DbError::Sqlite(error))
        }
    }
}

/// Highest version stored in a one-row-per-version table; zero when the table is absent or empty.
fn version_row_max(conn: &Connection, table: &str, version_column: &str) -> Result<i64> {
    check_ident(table)?;
    check_ident(version_column)?;
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1 COLLATE NOCASE)",
        [table],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(0);
    }
    let highest: Option<i64> = conn.query_row(
        &format!("SELECT MAX(CAST(\"{version_column}\" AS INTEGER)) FROM \"{table}\""),
        [],
        |row| row.get(0),
    )?;
    let version = highest.unwrap_or(0);
    if version < 0 {
        return Err(DbError::InvalidMigrations(format!(
            "invalid stored version {version}"
        )));
    }
    Ok(version)
}

/// Refuses any identifier that is not a plain ASCII name, before SQL is built from it.
fn check_storage(storage: &VersionStorage) -> Result<()> {
    match storage {
        VersionStorage::UserVersion => Ok(()),
        VersionStorage::Table { table, .. } => check_ident(table),
        VersionStorage::VersionTable {
            table,
            version_column,
            applied_at_column,
        } => {
            check_ident(table)?;
            check_ident(version_column)?;
            check_ident(applied_at_column)
        }
    }
}

/// Where the pre-migration backup goes: `<db>.pre-v<target>.bak`, or the name from
/// [`MigrateOptions::backup_name`]. The name must be a plain file name other than the database's.
fn backup_path(db: &Path, options: &MigrateOptions, current: i64, target: i64) -> Result<PathBuf> {
    let name = db
        .file_name()
        .ok_or(DbError::InvalidDatabasePath)?
        .to_string_lossy()
        .into_owned();
    let file = match options.backup_name {
        Some(namer) => namer(current, target),
        None => format!("{name}.pre-v{target}.bak"),
    };
    if file.is_empty() || file.contains(['/', '\\', '\0']) || file == "." || file == ".." {
        return Err(DbError::Backup(format!(
            "backup name {file:?} must be a plain file name other than the database's"
        )));
    }
    // The backup is published with a replacing rename, so it must never land on the database,
    // a live sidecar, or a recovery file this crate keeps beside the database. Compared
    // case-insensitively: a case-insensitive filesystem treats those names as the same file.
    let lowered = file.to_lowercase();
    let reserved = [
        "",
        "-wal",
        "-shm",
        "-journal",
        ".migrating",
        ".restore-journal",
        ".store-lock",
        ".transition-journal",
        QUARANTINE_JOURNAL_SUFFIX,
    ]
    .iter()
    .any(|suffix| lowered == format!("{name}{suffix}").to_lowercase());
    if reserved {
        return Err(DbError::Backup(format!(
            "backup name {file:?} is the database, one of its sidecars or a recovery file; choose another name"
        )));
    }
    Ok(db.with_file_name(file))
}

/// Keeps the `keep` newest pre-migration backups of `db`, counting `newest` (the backup just
/// taken), and removes the older `<db>.pre-*.bak` files beside it. Best effort: a file that
/// cannot be removed is left in place.
fn prune_backups(db: &Path, newest: &Path, keep: usize) {
    let Some(name) = db.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return;
    };
    let dir = db
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let prefix = format!("{name}.pre-");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut older: Vec<(SystemTime, PathBuf)> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.as_path() != newest)
        .filter(|path| {
            path.file_name().is_some_and(|n| {
                let n = n.to_string_lossy();
                n.starts_with(&prefix) && n.ends_with(".bak")
            })
        })
        .filter_map(|path| {
            let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok()?;
            Some((modified, path))
        })
        .collect();
    // Newest first; the just-taken backup already fills one of the `keep` places.
    older.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    for (_, path) in older.into_iter().skip(keep.saturating_sub(1)) {
        let _ = std::fs::remove_file(path);
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn validate(migrations: &[Migration]) -> Result<()> {
    for (index, migration) in migrations.iter().enumerate() {
        let expected = index as i64 + 1;
        if migration.version != expected {
            return Err(DbError::InvalidMigrations(format!(
                "expected version {expected} at position {index}, found {} ({})",
                migration.version, migration.name
            )));
        }
    }
    Ok(())
}

fn marker_path(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_os_string();
    name.push(".migrating");
    PathBuf::from(name)
}

pub fn pending_marker(db: &Path) -> Option<PendingMarker> {
    let text = std::fs::read_to_string(marker_path(db)).ok()?;
    let mut marker = PendingMarker {
        from: 0,
        to: 0,
        pid: 0,
    };
    for line in text.lines() {
        match line.split_once('=')? {
            ("from", v) => marker.from = v.parse().ok()?,
            ("to", v) => marker.to = v.parse().ok()?,
            ("pid", v) => marker.pid = v.parse().ok()?,
            _ => {}
        }
    }
    Some(marker)
}

/// Unix seconds when the pending `<db>.migrating` batch started, `0` for a marker written
/// before the start time was recorded, `None` without a readable marker. A separate reader keeps
/// [`PendingMarker`] constructible as before.
pub fn pending_marker_started_at(db: &Path) -> Option<u64> {
    pending_marker(db)?;
    let text = std::fs::read_to_string(marker_path(db)).ok()?;
    Some(
        text.lines()
            .find_map(|line| line.strip_prefix("started="))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
    )
}

/// Writes the `<db>.migrating` marker for a batch that moves the schema from version `from` to
/// `to`. The write is atomic and records the writing process id and the start time (unix
/// seconds). [`migrate`] writes this same marker; [`pending_marker`] reads it back. Use it to
/// simulate an interrupted migration in a crash test, instead of a private copy of the format.
pub fn write_pending_marker(db: &Path, from: i64, to: i64) -> Result<()> {
    let body = format!(
        "from={from}\nto={to}\npid={}\nstarted={}\n",
        std::process::id(),
        now_secs().max(0)
    );
    rightkit_fs::atomic_write(&marker_path(db), body.as_bytes())?;
    Ok(())
}

/// True when the version stored under `storage` is above the newest of `migrations`, which means
/// a newer build wrote this database. It only reads: nothing is migrated, created or written. It
/// does not consult [`MigrateOptions::import_legacy_versions`].
pub fn stored_version_is_newer(
    conn: &Connection,
    storage: &VersionStorage,
    migrations: &[Migration],
) -> Result<bool> {
    validate(migrations)?;
    check_storage(storage)?;
    let max = migrations.last().map_or(0, |m| m.version);
    Ok(migration_version(conn, storage)? > max)
}

pub fn migrate(
    conn: &Connection,
    db_path: Option<&Path>,
    migrations: &[Migration],
    options: &MigrateOptions,
) -> Result<MigrationReport> {
    validate(migrations)?;
    check_storage(&options.version_storage)?;
    let mut current = migration_version(conn, &options.version_storage)?;
    let max = migrations.last().map(|m| m.version).unwrap_or(0);
    let mut report = MigrationReport {
        from: current,
        to: current,
        ..Default::default()
    };
    report.interrupted_previous = db_path.and_then(pending_marker);

    // Legacy adoption happens only from version 0 (see `MigrateOptions::import_legacy_versions`).
    if let Some(legacy) = options.legacy_versions.as_ref().filter(|_| current == 0) {
        let legacy_version = version_row_max(conn, &legacy.table, &legacy.version_column)?;
        if legacy_version > 0 {
            if legacy_version <= max {
                adopt_legacy_version(conn, &options.version_storage, legacy_version)?;
            }
            current = legacy_version;
            report.from = current;
            report.to = current;
        }
    }

    if current > max {
        if options.forward_compatible.is_some_and(|probe| probe(conn)) {
            report.forward_compatible = true;
            return Ok(report);
        }
        return Err(DbError::NewerDatabase { current, max });
    }
    let pending: Vec<&Migration> = migrations.iter().filter(|m| m.version > current).collect();
    if pending.is_empty() {
        return Ok(report);
    }

    if current > 0 && options.backup {
        if let Some(path) = db_path {
            let backup = backup_path(path, options, current, max)?;
            let backup_options = options
                .backup_options
                .clone()
                .unwrap_or_else(|| DbOptions::new(path));
            crate::online_backup_to(&backup_options, conn, &backup, None)?;
            if let Some(verify) = options.verify_backup {
                verify(&backup).map_err(|reason| {
                    DbError::Backup(format!("pre-migration backup failed its check: {reason}"))
                })?;
            }
            if let Some(keep) = options.backup_retention {
                prune_backups(path, &backup, keep);
            }
            report.backup = Some(backup);
        }
    }

    if let Some(path) = db_path {
        write_pending_marker(path, current, max)?;
    }

    for migration in pending {
        conn.execute_batch("BEGIN IMMEDIATE;")?;
        let applied = match &migration.up {
            Step::Sql(sql) => conn.execute_batch(sql),
            Step::Code(run) => run(conn),
        }
        .and_then(|()| advance_version(conn, &options.version_storage, migration.version));
        match applied {
            Ok(()) => {
                conn.execute_batch("COMMIT;")?;
                report.applied.push(migration.version);
                report.to = migration.version;
            }
            Err(source) => {
                let _ = conn.execute_batch("ROLLBACK;");
                return Err(DbError::MigrationFailed {
                    version: migration.version,
                    name: migration.name,
                    source,
                });
            }
        }
    }

    if let Some(path) = db_path {
        let _ = std::fs::remove_file(marker_path(path));
    }
    if options.optimize_after {
        crate::optimize_boundary(conn)?;
    }
    Ok(report)
}

/// Revert to `target` by replaying each step's `down`, highest first, one transaction per step.
/// Refuses before touching anything unless every step in the range has a `down`.
pub fn rollback_to(conn: &Connection, migrations: &[Migration], target: i64) -> Result<Vec<i64>> {
    rollback_to_with_storage(conn, migrations, target, &VersionStorage::UserVersion)
}

/// Revert using the same version storage selected for [`migrate`].
pub fn rollback_to_with_storage(
    conn: &Connection,
    migrations: &[Migration],
    target: i64,
    storage: &VersionStorage,
) -> Result<Vec<i64>> {
    validate(migrations)?;
    check_storage(storage)?;
    if target < 0 {
        return Err(DbError::Rollback(
            "target version must be nonnegative".into(),
        ));
    }
    let current = migration_version(conn, storage)?;
    if current <= target {
        return Err(DbError::Rollback(format!(
            "already at v{current}, at or below target v{target}"
        )));
    }
    let mut plan = Vec::new();
    for version in ((target + 1)..=current).rev() {
        let migration = migrations
            .iter()
            .find(|m| m.version == version)
            .ok_or_else(|| DbError::Rollback(format!("v{version} is not a known migration")))?;
        let down = migration.down.ok_or_else(|| {
            DbError::Rollback(format!(
                "v{version} ({}) has no down migration",
                migration.name
            ))
        })?;
        plan.push((version, down));
    }
    let mut reverted = Vec::new();
    for (version, down) in plan {
        conn.execute_batch("BEGIN IMMEDIATE;")?;
        let result = conn
            .execute_batch(down)
            .and_then(|()| retreat_version(conn, storage, version));
        match result {
            Ok(()) => {
                conn.execute_batch("COMMIT;")?;
                reverted.push(version);
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK;");
                return Err(DbError::Rollback(format!(
                    "down v{version} failed and was rolled back: {error}"
                )));
            }
        }
    }
    Ok(reverted)
}

/// True if `table` has `column`, including STORED/VIRTUAL generated columns (`table_xinfo`;
/// `table_info` omits them, which made MailRight re-run an ALTER on every open).
pub fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    check_ident(table)?;
    let mut statement = conn.prepare(&format!("PRAGMA table_xinfo({table})"))?;
    let names = statement.query_map([], |row| row.get::<_, String>(1))?;
    for name in names {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Idempotent `ALTER TABLE .. ADD COLUMN` (SQLite has no `IF NOT EXISTS` for columns).
/// `table`, `column` and `decl` must be literals from the app, never user input.
pub fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    decl: &str,
) -> Result<()> {
    check_ident(table)?;
    check_ident(column)?;
    if !column_exists(conn, table, column)? {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl};"))?;
    }
    Ok(())
}

fn check_ident(ident: &str) -> Result<()> {
    if !ident.is_empty()
        && ident
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        Ok(())
    } else {
        Err(DbError::InvalidMigrations(format!(
            "unsafe SQL identifier {ident:?}"
        )))
    }
}

/// What [`DbOptions::open_writer_migrated_or_degrade`] returns.
#[derive(Debug)]
pub struct OpenOrDegrade {
    /// The writer to use: the database at the path, or a private in-memory store when degraded.
    pub conn: Connection,
    /// The migration report for the connection in `conn`.
    pub report: MigrationReport,
    /// `Some` when `conn` is not the database as it was stored: it was quarantined and recreated,
    /// or the app runs on an in-memory store. The app should show it to the user.
    pub degraded: Option<Degraded>,
}

/// Why [`DbOptions::open_writer_migrated_or_degrade`] did not use the stored database as-is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Degraded {
    /// The failure that made the stored database unusable.
    pub reason: String,
    /// Where the unusable file went, with its sidecars, when it was quarantined. Also set when a
    /// quarantine stopped part way and could not be undone (see `quarantine_incomplete`): then
    /// it is where the main file is now.
    pub quarantined: Option<PathBuf>,
    /// `true` when a quarantine moved part of the stored database and neither finished nor rolled
    /// back. The files are split between the original path and `quarantined`; a
    /// `<db>.quarantine-journal` file records the move and makes later opens through
    /// [`DbOptions::open_writer_migrated_or_degrade`] stay in memory instead of creating a
    /// replacement over the split files. A person must reunite or remove them.
    pub quarantine_incomplete: bool,
    /// `true` when the connection is a private in-memory store, not a file at the path.
    pub in_memory: bool,
}

impl DbOptions {
    /// Open the writer and bring the schema up to date (see [`migrate`]); the pre-migration
    /// backup is keyed like the database itself.
    pub fn open_writer_migrated(
        &self,
        migrations: &[Migration],
        options: &MigrateOptions,
    ) -> Result<(Connection, MigrationReport)> {
        let conn = self.open_writer()?;
        let mut options = options.clone();
        options.backup_options.get_or_insert_with(|| self.clone());
        let report = match migrate(&conn, Some(&self.path), migrations, &options) {
            Ok(report) => report,
            Err(err) => {
                // Corruption can first surface here, after `open_at` re-enabled the close
                // checkpoint. Closing this connection must not checkpoint or delete the `-wal`
                // before `open_writer_migrated_or_degrade` moves the store aside (rightkit#932).
                let _ = conn.set_db_config(
                    rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
                    true,
                );
                return Err(err);
            }
        };
        Ok((conn, report))
    }

    /// Open-or-degrade: [`Self::open_writer_migrated`], with recovery for a stored database that
    /// is corrupt, not a database, or fails its integrity check, at open or during migration.
    ///
    /// For such a failure, `policy` decides as in [`Self::open_writer_recovering`]: when it allows
    /// (`quarantine_corrupt` for a plain database, `quarantine_keyed` for a keyed one), the file
    /// and its sidecars move aside under the policy's stamp and sidecar options (never deleted by
    /// default), and the database is created and migrated again. When it does not allow, the file
    /// is left untouched. Either way, if the store still cannot be opened, the connection is a
    /// private in-memory store that is migrated the same way only when
    /// `policy.allow_in_memory_fallback` explicitly acknowledges it (default refuse: the
    /// original error is returned). When acknowledged, [`OpenOrDegrade::degraded`] says so
    /// with `in_memory: true`, and the configured key remains applied.
    ///
    /// Any other failure (busy, permission, I/O, a newer database, an invalid migration list) is
    /// returned as an error, because it does not show the stored data is damaged.
    pub fn open_writer_migrated_or_degrade(
        &self,
        migrations: &[Migration],
        options: &MigrateOptions,
        policy: RecoveryPolicy,
    ) -> Result<OpenOrDegrade> {
        // A quarantine an earlier run could not finish or undo: never create a replacement over
        // the split files.
        if let Some(journal) = unfinished_quarantine(&self.path) {
            let reason = format!(
                "an earlier quarantine of {} stopped part way and was not undone; see {}",
                self.path.display(),
                journal.display()
            );
            let refuse = DbError::Fs(rightkit_fs::FsError::io(
                &journal,
                std::io::Error::other(reason.clone()),
            ));
            return self.degrade_in_memory(migrations, options, policy, reason, None, true, refuse);
        }
        let error = match self.open_writer_migrated(migrations, options) {
            Ok((conn, report)) => {
                return Ok(OpenOrDegrade {
                    conn,
                    report,
                    degraded: None,
                })
            }
            Err(error) if is_recoverable_damage(&error) => error,
            Err(error) => return Err(error),
        };
        let reason = error.to_string();
        if !allowed(self, policy) {
            return self.degrade_in_memory(migrations, options, policy, reason, None, false, error);
        }
        let quarantined = match quarantine_database_journaled(
            &self.path,
            policy.quarantine_stamp,
            policy.sidecars,
        ) {
            Ok(moved) => moved,
            Err(failure) => {
                // Nothing moved (or every move was undone): the store is untouched. Otherwise
                // the split stays on disk, journaled, and the result says so.
                let reason = format!("{reason}; quarantine failed: {}", failure.error);
                let incomplete = failure.stranded_at.is_some();
                return self.degrade_in_memory(
                    migrations,
                    options,
                    policy,
                    reason,
                    failure.stranded_at,
                    incomplete,
                    failure.error,
                );
            }
        };
        match self.open_writer_migrated(migrations, options) {
            Ok((conn, report)) => Ok(OpenOrDegrade {
                conn,
                report,
                degraded: Some(Degraded {
                    reason,
                    quarantined: Some(quarantined),
                    quarantine_incomplete: false,
                    in_memory: false,
                }),
            }),
            Err(second) => {
                let reason = format!("{reason}; reopen after quarantine failed: {second}");
                self.degrade_in_memory(
                    migrations,
                    options,
                    policy,
                    reason,
                    Some(quarantined),
                    false,
                    second,
                )
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn degrade_in_memory(
        &self,
        migrations: &[Migration],
        options: &MigrateOptions,
        policy: RecoveryPolicy,
        reason: String,
        quarantined: Option<PathBuf>,
        quarantine_incomplete: bool,
        refuse: DbError,
    ) -> Result<OpenOrDegrade> {
        // In-memory recovery is never automatic: without explicit acknowledgement the
        // caller gets the original failure, not a store that will not persist.
        if !policy.allow_in_memory_fallback {
            return Err(refuse);
        }
        let conn = self.open_in_memory()?;
        let report = migrate(&conn, None, migrations, options)?;
        Ok(OpenOrDegrade {
            conn,
            report,
            degraded: Some(Degraded {
                reason,
                quarantined,
                quarantine_incomplete,
                in_memory: true,
            }),
        })
    }
}

/// Damage that recovery may act on: an unreadable or failed-integrity open (see
/// [`is_unreadable`]), or a migration step that SQLite reports as not a database or corrupt.
fn is_recoverable_damage(error: &DbError) -> bool {
    match error {
        DbError::MigrationFailed {
            source: rusqlite::Error::SqliteFailure(failure, _),
            ..
        } => matches!(
            failure.code,
            ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt
        ),
        other => is_unreadable(other),
    }
}
