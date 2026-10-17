//! Borrowed-connection settings in a SQL `settings(key, value)` table (feature `sql`).
//!
//! [`SqlSettings`] owns no connection, pool or file. Every call takes the caller's
//! `&rusqlite::Connection`, which is also what a `&rusqlite::Transaction` derefs to, so the same
//! call runs inside the app's own migration, `BEGIN ... COMMIT` block or savepoint and joins it:
//! nothing here commits, rolls back or begins a transaction the caller did not start. The
//! connection may be an encrypted SQLCipher one (build `rightkit-db` with a `sqlcipher*` feature);
//! the crate does not open or key anything.
//!
//! ## Shape
//! - One table, default `settings`, with a text key column `key` and a text value column `value`.
//!   The key column must be the primary key or carry a unique index (writes upsert). Table and
//!   column names are configurable so an existing app keeps its names and key strings unchanged;
//!   [`SqlSettings::ensure_table`] creates the default shape if the app does not already own it.
//! - Keys are free-form strings, so dynamic keys such as `signature:<account>` need no
//!   registration. [`SqlSettings::list`] reads every key under a prefix.
//! - Values are strings. Typing and validation stay with the caller; a string default per key can
//!   be configured.
//!
//! ## Version, migrations, refusal
//! Name a version key with [`SqlSettings::version_key`] to opt in. The stored value is the schema
//! version as decimal text (no version row means version 0). [`SqlSettings::migrate`] runs the
//! [`Schema`]'s migrations over the whole table as a JSON object of string values, then writes
//! the difference back and stamps the version, all inside one savepoint.
//!
//! Data newer than the code is never reset or overwritten, the same promise as
//! [`crate::OnRefused::ReadOnlyDefaults`]: while the stored version is above the schema's,
//! [`SqlSettings::get`] returns only the configured defaults, [`SqlSettings::refusal`] says why,
//! and every write and `migrate` fail with [`Error::NewerSchema`] and touch nothing. A missing
//! table is "no settings yet": reads return the defaults and the app creates the table in its own
//! migration; writes then fail with the SQLite error.

use crate::store::{Error, Refusal, Schema};
use rightkit_db::rusqlite::{self, Connection, OptionalExtension};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Failure of a SQL settings call.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SqlError {
    /// SQLite refused the statement (including a missing table on a write, or a locked database).
    #[error("settings sql: {0}")]
    Sql(#[from] rusqlite::Error),
    /// A settings-level refusal: [`Error::NewerSchema`], [`Error::Invalid`] (an unreadable version
    /// value), or an error a migration returned.
    #[error("{0}")]
    Settings(#[from] Error),
    /// A table or column name is not a plain identifier (`[A-Za-z_][A-Za-z0-9_]*`).
    #[error("invalid SQL identifier {0:?}")]
    BadIdentifier(String),
}

/// A settings table accessed through borrowed connections. Cheap to clone; holds no connection.
#[derive(Clone)]
pub struct SqlSettings {
    schema: Schema,
    table: String,
    key_column: String,
    value_column: String,
    version_key: Option<String>,
    defaults: BTreeMap<String, String>,
}

impl SqlSettings {
    /// Table `settings`, columns `key` and `value`, no version tracking, no defaults.
    pub fn new(schema: Schema) -> Self {
        Self {
            schema,
            table: "settings".into(),
            key_column: "key".into(),
            value_column: "value".into(),
            version_key: None,
            defaults: BTreeMap::new(),
        }
    }

    /// Use another table name.
    pub fn table(mut self, name: &str) -> Result<Self, SqlError> {
        check_ident(name)?;
        self.table = name.into();
        Ok(self)
    }

    /// Use other key and value column names.
    pub fn columns(mut self, key: &str, value: &str) -> Result<Self, SqlError> {
        check_ident(key)?;
        check_ident(value)?;
        self.key_column = key.into();
        self.value_column = value.into();
        Ok(self)
    }

    /// Keep the schema version in this key of the same table, enabling [`Self::migrate`] and the
    /// newer-data refusal. Without it no version row is read or written.
    pub fn version_key(mut self, key: impl Into<String>) -> Self {
        self.version_key = Some(key.into());
        self
    }

    /// The value [`Self::get`] returns for a key with no stored row.
    pub fn default_value(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.defaults.insert(key.into(), value.into());
        self
    }

    /// Create the default-shaped table if it does not exist. A no-op on an existing table.
    pub fn ensure_table(&self, conn: &Connection) -> Result<(), SqlError> {
        conn.execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS \"{}\" (\"{}\" TEXT PRIMARY KEY, \"{}\" TEXT NOT NULL)",
            self.table, self.key_column, self.value_column
        ))?;
        Ok(())
    }

    /// Why this connection's data is refused, if it is: a stored version above the schema's.
    /// `None` when no version key is configured, no version is stored, or the table is missing.
    pub fn refusal(&self, conn: &Connection) -> Result<Option<Refusal>, SqlError> {
        match self.stored_version(conn)? {
            Some(found) if found > self.schema.version => Ok(Some(Refusal::NewerSchema {
                found,
                supported: self.schema.version,
            })),
            _ => Ok(None),
        }
    }

    /// `true` unless the data is refused (see [`Self::refusal`]).
    pub fn writable(&self, conn: &Connection) -> Result<bool, SqlError> {
        Ok(self.refusal(conn)?.is_none())
    }

    /// The stored value, else the configured default, else `None`. While the data is refused,
    /// only the default is returned.
    pub fn get(&self, conn: &Connection, key: &str) -> Result<Option<String>, SqlError> {
        if self.refusal(conn)?.is_some() {
            return Ok(self.defaults.get(key).cloned());
        }
        Ok(self
            .read(conn, key)?
            .or_else(|| self.defaults.get(key).cloned()))
    }

    /// Every stored `(key, value)` whose key starts with `prefix` (empty prefix: all), ordered by
    /// key. Does not include defaults. Empty while the data is refused. The version key is
    /// included if it matches the prefix.
    pub fn list(
        &self,
        conn: &Connection,
        prefix: &str,
    ) -> Result<Vec<(String, String)>, SqlError> {
        if self.refusal(conn)?.is_some() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT \"{k}\", \"{v}\" FROM \"{t}\" WHERE substr(\"{k}\", 1, length(?1)) = ?1 ORDER BY \"{k}\"",
            k = self.key_column,
            v = self.value_column,
            t = self.table
        );
        self.collect_rows(conn, &sql, [prefix])
    }

    /// Insert or replace one key.
    pub fn set(&self, conn: &Connection, key: &str, value: &str) -> Result<(), SqlError> {
        self.require_writable(conn)?;
        self.upsert(conn, key, value)
    }

    /// Delete one key. `Ok(true)` if a row was removed.
    pub fn remove(&self, conn: &Connection, key: &str) -> Result<bool, SqlError> {
        self.require_writable(conn)?;
        Ok(self.delete(conn, key)? > 0)
    }

    /// Apply several edits together: `Some(value)` sets, `None` removes. All or none, inside a
    /// savepoint that joins the caller's transaction (or is its own when none is open).
    pub fn patch(&self, conn: &Connection, edits: &[(&str, Option<&str>)]) -> Result<(), SqlError> {
        self.require_writable(conn)?;
        in_savepoint(conn, || {
            for (key, value) in edits {
                match value {
                    Some(v) => self.upsert(conn, key, v)?,
                    None => {
                        self.delete(conn, key)?;
                    }
                }
            }
            Ok(())
        })
    }

    /// Bring the table to the schema's version and return that version. Requires
    /// [`Self::version_key`]. Fails with [`Error::NewerSchema`] and writes nothing when the stored
    /// version is newer. Runs the pending migrations in order over the string values (a migration
    /// may set a key to a non-string JSON value, stored as its JSON text, or to `null` to delete
    /// it), writes only what changed, and stamps the version, atomically. Already current: no
    /// write.
    pub fn migrate(&self, conn: &Connection) -> Result<u32, SqlError> {
        self.schema.validate()?;
        let Some(vkey) = self.version_key.as_deref() else {
            return Err(Error::BadSchema("migrate needs version_key").into());
        };
        let current = self.stored_version(conn)?;
        if let Some(found) = current {
            if found > self.schema.version {
                return Err(Error::NewerSchema {
                    found,
                    supported: self.schema.version,
                }
                .into());
            }
            if found == self.schema.version {
                return Ok(found);
            }
        }
        let from = current.unwrap_or(0);
        let sql = format!(
            "SELECT \"{k}\", \"{v}\" FROM \"{t}\"",
            k = self.key_column,
            v = self.value_column,
            t = self.table
        );
        let before: BTreeMap<String, String> = self
            .collect_rows(conn, &sql, rusqlite::params![])?
            .into_iter()
            .filter(|(k, _)| k != vkey)
            .collect();
        let mut values: Map<String, Value> = before
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        for m in self.schema.migrations.iter().filter(|m| m.to > from) {
            (m.apply)(&mut values)?;
        }
        let after: BTreeMap<String, String> = values
            .into_iter()
            .filter_map(|(k, v)| match v {
                Value::Null => None,
                Value::String(s) => Some((k, s)),
                other => Some((k, other.to_string())),
            })
            .collect();
        in_savepoint(conn, || {
            for k in before.keys().filter(|k| !after.contains_key(*k)) {
                self.delete(conn, k)?;
            }
            for (k, v) in &after {
                if before.get(k) != Some(v) {
                    self.upsert(conn, k, v)?;
                }
            }
            self.upsert(conn, vkey, &self.schema.version.to_string())
        })?;
        Ok(self.schema.version)
    }

    fn require_writable(&self, conn: &Connection) -> Result<(), SqlError> {
        match self.refusal(conn)? {
            Some(r) => Err(Error::from(r).into()),
            None => Ok(()),
        }
    }

    fn stored_version(&self, conn: &Connection) -> Result<Option<u32>, SqlError> {
        let Some(key) = self.version_key.as_deref() else {
            return Ok(None);
        };
        match self.read(conn, key)? {
            None => Ok(None),
            Some(text) => text.trim().parse::<u32>().map(Some).map_err(|_| {
                Error::Invalid(format!("stored schema version {text:?} is not a number")).into()
            }),
        }
    }

    fn read(&self, conn: &Connection, key: &str) -> Result<Option<String>, SqlError> {
        let sql = format!(
            "SELECT \"{v}\" FROM \"{t}\" WHERE \"{k}\" = ?1",
            k = self.key_column,
            v = self.value_column,
            t = self.table
        );
        match conn
            .query_row(&sql, [key], |r| r.get::<_, Option<String>>(0))
            .optional()
        {
            Ok(v) => Ok(v.flatten()),
            Err(e) if no_such_table(&e) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Run a two-column `(key, value)` query; a missing table is an empty result, a NULL value
    /// is skipped.
    fn collect_rows<P: rusqlite::Params>(
        &self,
        conn: &Connection,
        sql: &str,
        params: P,
    ) -> Result<Vec<(String, String)>, SqlError> {
        let mut out = Vec::new();
        let mut stmt = match conn.prepare(sql) {
            Ok(stmt) => stmt,
            Err(e) if no_such_table(&e) => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        let rows = stmt.query_map(params, |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        for row in rows {
            let (k, v) = row?;
            if let Some(v) = v {
                out.push((k, v));
            }
        }
        Ok(out)
    }

    fn upsert(&self, conn: &Connection, key: &str, value: &str) -> Result<(), SqlError> {
        conn.execute(
            &format!(
                "INSERT INTO \"{t}\" (\"{k}\", \"{v}\") VALUES (?1, ?2) \
                 ON CONFLICT(\"{k}\") DO UPDATE SET \"{v}\" = excluded.\"{v}\"",
                k = self.key_column,
                v = self.value_column,
                t = self.table
            ),
            [key, value],
        )?;
        Ok(())
    }

    fn delete(&self, conn: &Connection, key: &str) -> Result<usize, SqlError> {
        Ok(conn.execute(
            &format!(
                "DELETE FROM \"{t}\" WHERE \"{k}\" = ?1",
                k = self.key_column,
                t = self.table
            ),
            [key],
        )?)
    }
}

impl From<Refusal> for Error {
    fn from(r: Refusal) -> Self {
        match r {
            Refusal::NewerSchema { found, supported } => Error::NewerSchema { found, supported },
            Refusal::Oversized { len, limit } => Error::Invalid(format!(
                "settings file is {len} bytes, over the {limit} byte limit"
            )),
        }
    }
}

fn check_ident(name: &str) -> Result<(), SqlError> {
    let mut chars = name.chars();
    let ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok {
        Ok(())
    } else {
        Err(SqlError::BadIdentifier(name.into()))
    }
}

fn no_such_table(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(_, Some(msg)) if msg.starts_with("no such table"))
}

/// Run `f` inside a savepoint: released on success, rolled back on error. A savepoint nests in
/// the caller's transaction or savepoint and starts its own transaction when none is open.
fn in_savepoint<T>(
    conn: &Connection,
    f: impl FnOnce() -> Result<T, SqlError>,
) -> Result<T, SqlError> {
    conn.execute_batch("SAVEPOINT rightkit_settings")?;
    match f() {
        Ok(v) => {
            conn.execute_batch("RELEASE rightkit_settings")?;
            Ok(v)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK TO rightkit_settings; RELEASE rightkit_settings");
            Err(e)
        }
    }
}
