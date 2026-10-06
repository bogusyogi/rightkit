//! Ordered, transactional, crash-aware schema migrations.
//!
//! The library owns the *mechanism*; apps own the schemas. Rules, taken from the strongest
//! donors (CodeRight `storage::migrations`, MailRight `db::migrations`, WorkRight `local-store`):
//!
//! - versions are contiguous from 1 and tracked in `PRAGMA user_version`;
//! - each step runs in its own `BEGIN IMMEDIATE` transaction together with its version bump, so
//!   a failing step rolls back to the last good version;
//! - before the first pending step on a non-empty database an online backup is taken to
//!   `<db>.pre-v<target>.bak` (WAL-inclusive, atomic, owner-only);
//! - a `<db>.migrating` marker brackets the batch so a crash mid-batch is reported on next open;
//! - a database newer than this build is refused unless the app's probe says the subset it
//!   needs is still present (additive forward compatibility);
//! - rollback replays `down` steps and refuses when any step in the range lacks one.

use crate::{DbError, DbOptions, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

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
        Self { version, name, up: Step::Sql(up), down: None }
    }

    pub const fn with_down(mut self, down: &'static str) -> Self {
        self.down = Some(down);
        self
    }

    pub const fn code(version: i64, name: &'static str, up: fn(&Connection) -> rusqlite::Result<()>) -> Self {
        Self { version, name, up: Step::Code(up), down: None }
    }
}

#[derive(Clone)]
pub struct MigrateOptions {
    /// Take a pre-migration backup when migrating an existing (version > 0) on-disk database.
    pub backup: bool,
    /// Key source for the backup (SQLCipher); `DbOptions::open_writer_migrated` fills this in.
    pub backup_options: Option<DbOptions>,
    /// For a database newer than this build: `true` means the subset this build reads/writes is
    /// intact, so open it read-write without migrating.
    pub forward_compatible: Option<fn(&Connection) -> bool>,
    /// Refresh planner statistics after migrating (`PRAGMA optimize`).
    pub optimize_after: bool,
}

impl Default for MigrateOptions {
    fn default() -> Self {
        Self { backup: true, backup_options: None, forward_compatible: None, optimize_after: true }
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
    let mut marker = PendingMarker { from: 0, to: 0, pid: 0 };
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

pub fn migrate(
    conn: &Connection,
    db_path: Option<&Path>,
    migrations: &[Migration],
    options: &MigrateOptions,
) -> Result<MigrationReport> {
    validate(migrations)?;
    let current = user_version(conn)?;
    let max = migrations.last().map(|m| m.version).unwrap_or(0);
    let mut report = MigrationReport { from: current, to: current, ..Default::default() };
    report.interrupted_previous = db_path.and_then(pending_marker);

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
            let name = path.file_name().ok_or(DbError::InvalidDatabasePath)?.to_string_lossy();
            let backup = path.with_file_name(format!("{name}.pre-v{max}.bak"));
            let backup_options = options.backup_options.clone().unwrap_or_else(|| DbOptions::new(path));
            crate::online_backup_to(&backup_options, conn, &backup, None)?;
            report.backup = Some(backup);
        }
    }

    if let Some(path) = db_path {
        let body = format!("from={current}\nto={max}\npid={}\n", std::process::id());
        rightkit_fs::atomic_write(&marker_path(path), body.as_bytes())?;
    }

    for migration in pending {
        conn.execute_batch("BEGIN IMMEDIATE;")?;
        let applied = match &migration.up {
            Step::Sql(sql) => conn.execute_batch(sql),
            Step::Code(run) => run(conn),
        }
        .and_then(|()| set_user_version(conn, migration.version));
        match applied {
            Ok(()) => {
                conn.execute_batch("COMMIT;")?;
                report.applied.push(migration.version);
                report.to = migration.version;
            }
            Err(source) => {
                let _ = conn.execute_batch("ROLLBACK;");
                return Err(DbError::MigrationFailed { version: migration.version, name: migration.name, source });
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
    validate(migrations)?;
    let current = user_version(conn)?;
    if current <= target {
        return Err(DbError::Rollback(format!("already at v{current}, at or below target v{target}")));
    }
    let mut plan = Vec::new();
    for version in ((target + 1)..=current).rev() {
        let migration = migrations
            .iter()
            .find(|m| m.version == version)
            .ok_or_else(|| DbError::Rollback(format!("v{version} is not a known migration")))?;
        let down = migration
            .down
            .ok_or_else(|| DbError::Rollback(format!("v{version} ({}) has no down migration", migration.name)))?;
        plan.push((version, down));
    }
    let mut reverted = Vec::new();
    for (version, down) in plan {
        conn.execute_batch("BEGIN IMMEDIATE;")?;
        let result = conn.execute_batch(down).and_then(|()| set_user_version(conn, version - 1));
        match result {
            Ok(()) => {
                conn.execute_batch("COMMIT;")?;
                reverted.push(version);
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK;");
                return Err(DbError::Rollback(format!("down v{version} failed and was rolled back: {error}")));
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
pub fn add_column_if_missing(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    check_ident(table)?;
    check_ident(column)?;
    if !column_exists(conn, table, column)? {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl};"))?;
    }
    Ok(())
}

fn check_ident(ident: &str) -> Result<()> {
    if !ident.is_empty() && ident.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        Ok(())
    } else {
        Err(DbError::InvalidMigrations(format!("unsafe SQL identifier {ident:?}")))
    }
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
        let report = migrate(&conn, Some(&self.path), migrations, &options)?;
        Ok((conn, report))
    }
}
