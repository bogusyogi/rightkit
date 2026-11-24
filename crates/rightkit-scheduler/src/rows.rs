//! Schedule records as neutral rows (ME-4: schedules sync as a family).
//!
//! The runner ([`crate::runner`]) decides which minutes are due; this module owns where a schedule's record
//! lives in the same SQLite file: the `schedules` table, with the columns a host already keeps (`id`, `label`,
//! `spec`, `misfire`, `shown`, `digest`, `created_ms`, `confirmed_ms`) plus `updated_ms`, the version a synced
//! copy is ordered by. [`ScheduleRow`] is the neutral item: serde-serialisable, so a host can carry it as the
//! body of a sync record, and read and written only through [`list_rows`], [`get_row`], [`upsert_row`] and
//! [`delete_row`].
//!
//! A row carries `confirmed_ms`, the owner's confirmation. That is deliberate: a schedule the owner
//! confirmed on one device is confirmed on his others once it syncs. It is safe only because sync runs
//! between the owner's own devices behind end-to-end encryption, and the host must never apply a row
//! it did not get that way.

use rightkit_db::rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::model::ScheduleSpec;
use crate::profile::MisfirePolicy;
use crate::runner::RunnerError;

/// Longest id, in bytes.
pub const MAX_ROW_ID_BYTES: usize = 128;
/// Longest label, in characters.
pub const MAX_ROW_LABEL_CHARS: usize = 80;

/// One schedule record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleRow {
    pub id: String,
    pub label: String,
    pub spec: ScheduleSpec,
    pub misfire: MisfirePolicy,
    /// The next run times (Unix seconds) the owner was shown when the schedule was added.
    pub shown: Vec<i64>,
    /// A digest of what was shown; a confirmation binds to it.
    pub digest: String,
    pub created_ms: i64,
    /// Unix milliseconds the owner confirmed it, or `None` while it waits. The runner claims nothing for an
    /// unconfirmed schedule.
    pub confirmed_ms: Option<i64>,
    /// Unix milliseconds of the last change; the version a synced copy is ordered by.
    pub updated_ms: i64,
}

/// The word a misfire policy is stored and spoken as: `skip`, `coalesce` or `catch_up`.
pub fn misfire_word(policy: MisfirePolicy) -> &'static str {
    match policy {
        MisfirePolicy::Skip => "skip",
        MisfirePolicy::Coalesce => "coalesce",
        MisfirePolicy::CatchUp => "catch_up",
    }
}

/// The policy a word names, or `None`.
pub fn misfire_of(word: &str) -> Option<MisfirePolicy> {
    match word {
        "skip" => Some(MisfirePolicy::Skip),
        "coalesce" => Some(MisfirePolicy::Coalesce),
        "catch_up" => Some(MisfirePolicy::CatchUp),
        _ => None,
    }
}

fn bad(what: &str) -> RunnerError {
    RunnerError::Spec(what.to_string())
}

/// Create the `schedules` table if it is missing and add `updated_ms` to one that lacks it (a table a host made
/// before this module existed). Idempotent; call it at every start. An added column is filled from
/// `confirmed_ms` or `created_ms`, so an old row has a version.
pub fn init_rows_table(conn: &Connection) -> Result<(), RunnerError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schedules (
             id           TEXT NOT NULL PRIMARY KEY,
             label        TEXT NOT NULL,
             spec         TEXT NOT NULL,
             misfire      TEXT NOT NULL,
             shown        TEXT NOT NULL,
             digest       TEXT NOT NULL,
             created_ms   INTEGER NOT NULL,
             confirmed_ms INTEGER,
             updated_ms   INTEGER NOT NULL DEFAULT 0
         ) WITHOUT ROWID;",
    )?;
    let has_updated: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('schedules') WHERE name = 'updated_ms'")?
        .exists([])?;
    if !has_updated {
        conn.execute_batch(
            "ALTER TABLE schedules ADD COLUMN updated_ms INTEGER NOT NULL DEFAULT 0;
             UPDATE schedules SET updated_ms = COALESCE(confirmed_ms, created_ms);",
        )?;
    }
    Ok(())
}

fn validate(row: &ScheduleRow) -> Result<(), RunnerError> {
    let id_ok = !row.id.is_empty()
        && row.id.len() <= MAX_ROW_ID_BYTES
        && !row.id.chars().any(char::is_control);
    if !id_ok {
        return Err(bad(
            "schedule id is empty, too long or has a control character",
        ));
    }
    if row.label.chars().count() > MAX_ROW_LABEL_CHARS || row.label.chars().any(char::is_control) {
        return Err(bad("schedule label is too long or has a control character"));
    }
    if row.digest.is_empty() || row.digest.len() > 64 {
        return Err(bad("schedule digest is empty or too long"));
    }
    row.spec.validate().map_err(RunnerError::Spec)
}

fn row_from(values: Raw) -> Result<ScheduleRow, RunnerError> {
    let (id, label, spec, misfire, shown, digest, created_ms, confirmed_ms, updated_ms) = values;
    Ok(ScheduleRow {
        id,
        label,
        spec: serde_json::from_str(&spec)
            .map_err(|_| bad("a stored schedule spec is not readable"))?,
        misfire: misfire_of(&misfire).ok_or_else(|| bad("a stored misfire policy is not known"))?,
        shown: serde_json::from_str(&shown)
            .map_err(|_| bad("a stored run list is not readable"))?,
        digest,
        created_ms,
        confirmed_ms,
        updated_ms,
    })
}

const COLUMNS: &str =
    "id, label, spec, misfire, shown, digest, created_ms, confirmed_ms, updated_ms";

type Raw = (
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    Option<i64>,
    i64,
);

fn raw_of(row: &rightkit_db::rusqlite::Row<'_>) -> rightkit_db::rusqlite::Result<Raw> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
    ))
}

/// Every schedule row, oldest first (then by id). A row that cannot be read is an error, not skipped.
pub fn list_rows(conn: &Connection) -> Result<Vec<ScheduleRow>, RunnerError> {
    let mut statement = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM schedules ORDER BY created_ms, id"
    ))?;
    let raws = statement
        .query_map([], raw_of)?
        .collect::<Result<Vec<Raw>, _>>()?;
    raws.into_iter().map(row_from).collect()
}

/// One schedule row by id.
pub fn get_row(conn: &Connection, id: &str) -> Result<Option<ScheduleRow>, RunnerError> {
    let raw = conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM schedules WHERE id = ?1"),
            params![id],
            raw_of,
        )
        .optional()?;
    raw.map(row_from).transpose()
}

/// Insert the row, or replace the one with its id. Returns whether anything changed: a row identical to the
/// one held changes nothing. `created_ms` of an existing row is kept (the oldest wins); every other field
/// is the new row's.
pub fn upsert_row(conn: &Connection, row: &ScheduleRow) -> Result<bool, RunnerError> {
    validate(row)?;
    let existing = get_row(conn, &row.id)?;
    let mut next = row.clone();
    if let Some(held) = &existing {
        next.created_ms = held.created_ms.min(row.created_ms);
        if *held == next {
            return Ok(false);
        }
    }
    let spec = serde_json::to_string(&next.spec)
        .map_err(|_| bad("the schedule spec cannot be written"))?;
    let shown =
        serde_json::to_string(&next.shown).map_err(|_| bad("the run list cannot be written"))?;
    conn.execute(
        "INSERT INTO schedules (id, label, spec, misfire, shown, digest, created_ms, confirmed_ms, updated_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(id) DO UPDATE SET
             label = excluded.label, spec = excluded.spec, misfire = excluded.misfire,
             shown = excluded.shown, digest = excluded.digest, created_ms = excluded.created_ms,
             confirmed_ms = excluded.confirmed_ms, updated_ms = excluded.updated_ms",
        params![
            next.id,
            next.label,
            spec,
            misfire_word(next.misfire),
            shown,
            next.digest,
            next.created_ms,
            next.confirmed_ms,
            next.updated_ms
        ],
    )?;
    Ok(true)
}

/// Delete a schedule row and the runner's cursor and claims for it, so the same id added again starts
/// fresh. Returns whether a row was there. The claim tables are cleared only if they exist.
pub fn delete_row(conn: &Connection, id: &str) -> Result<bool, RunnerError> {
    let removed = conn.execute("DELETE FROM schedules WHERE id = ?1", params![id])?;
    for table in ["schedule_cursor", "schedule_claims"] {
        let exists = conn
            .prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")?
            .exists(params![table])?;
        if exists {
            conn.execute(
                &format!("DELETE FROM {table} WHERE schedule_id = ?1"),
                params![id],
            )?;
        }
    }
    Ok(removed > 0)
}
