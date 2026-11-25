//! A read-only check of an existing SQLite store before an app opens it for writing: its schema
//! version and one required column. A store written by another build is refused and left
//! untouched, never upgraded; the owner moves it aside by hand.
//!
//! Ported from Pulse `core/src/budget.rs` (`refuse_unsupported_store`, #141): migrations were
//! folded into one final-form schema, so an older file is refused, not upgraded.

use std::path::Path;

use rightkit_db::rusqlite;

/// What [`check_store`] found.
#[derive(Debug)]
pub enum StoreCheck {
    /// Missing, empty, never initialised, or in the supported form: safe to open.
    Supported,
    /// Written by another build; the text says why. The file is untouched.
    Unsupported(String),
    /// The file could not be read as a SQLite store.
    Unreadable(rusqlite::Error),
}

/// Checks the file at `path` without changing, renaming or deleting it. `table` is the store's
/// main table (a file without it and with `user_version` 0 was never initialised), `version` the
/// one `PRAGMA user_version` this build reads, and `required` a `(table, column)` the final-form
/// schema must have.
pub fn check_store(path: &Path, table: &str, version: i64, required: (&str, &str)) -> StoreCheck {
    if std::fs::metadata(path)
        .map(|m| m.len() == 0)
        .unwrap_or(true)
    {
        return StoreCheck::Supported;
    }
    match probe(path, table, version, required) {
        Ok(check) => check,
        Err(error) => StoreCheck::Unreadable(error),
    }
}

fn probe(
    path: &Path,
    table: &str,
    version: i64,
    required: (&str, &str),
) -> rusqlite::Result<StoreCheck> {
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let found: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let has_table: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )?;
    if !has_table && found == 0 {
        return Ok(StoreCheck::Supported);
    }
    if found != version {
        return Ok(StoreCheck::Unsupported(format!(
            "schema version {found}, this build reads {version}"
        )));
    }
    let has_column: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2)",
        [required.0, required.1],
        |row| row.get(0),
    )?;
    if !has_column {
        return Ok(StoreCheck::Unsupported(format!(
            "its {} table has no {} column",
            required.0, required.1
        )));
    }
    Ok(StoreCheck::Supported)
}
