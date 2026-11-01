//! `rightkit-library`: a local artifact library. A SQLite-backed history/search index
//! ([`Store`]) plus a deterministic, non-LLM enrichment pass ([`enrichment`]).
//!
//! Files (txt, srt, slide_dir) remain the canonical source of truth; this crate
//! is a cache/index only.  All persistence is through a single `transcripts`
//! table keyed on `id TEXT PRIMARY KEY`.
//!
//! # Minimal example
//! ```rust
//! use rightkit_library::{Store, HistoryRecord};
//!
//! let store = Store::open_in_memory();
//! let rec = HistoryRecord {
//!     id: "abc123".into(),
//!     url: "https://example.com/clip".into(),
//!     kind: "youtube".into(),
//!     title: Some("My Clip".into()),
//!     lang: Some("en".into()),
//!     duration_s: Some(42.0),
//!     created_at: "2026-05-29T00:00:00Z".into(),
//!     txt_path: None,
//!     md_path: None,
//!     srt_path: None,
//!     slide_dir: None,
//!     tags: None,
//!     snippet: None,
//!     media_path: None,
//!     media_meta: None,
//!     media_action: None,
//!     srt_text: None,
//!     truncated_at_s: None,
//! };
//! store.upsert(&rec).unwrap();
//! assert!(store.get("abc123").unwrap().is_some());
//! ```

pub mod artifacts;
pub mod enrichment;
pub mod history;
pub mod history_types;
mod path_policy;

pub use history_types::{
    HistoryPage, HistoryRow, HistorySettings, HistorySort, Kind, TranscriptDetail,
    TranscriptImport, TranscriptVariant, TranscriptVariantInfo, TranscriptVariantKind,
    HISTORY_PAGE_MAX, HISTORY_PAGE_SIZE,
};

use std::path::Path;

use rightkit_db::rusqlite::{named_params, params, Connection, Error as SqlError, Result, Row};
use rightkit_db::{
    add_column_if_missing, migrate, migration_version, DbError, DbOptions, DurabilityProfile,
    MigrateOptions, Migration, VersionStorage,
};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A single history entry in the transcript index.
///
/// Mirrors the app's history row wire type, with the
/// additions of `slide_dir` and `tags` which live only in the store layer.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryRecord {
    pub id: String,
    pub url: String,
    /// Serialised kind string, e.g. `"youtube"`, `"reel"`, `"local_file"`.
    pub kind: String,
    pub title: Option<String>,
    pub lang: Option<String>,
    pub duration_s: Option<f64>,
    /// Creation timestamp as a string. The queue writes epoch-seconds (e.g.
    /// `"1748995200"`); ISO-8601 (e.g. `"2026-05-29T12:00:00Z"`) is also
    /// accepted by the UI. Stored verbatim; not parsed here.
    pub created_at: String,
    pub txt_path: Option<String>,
    pub md_path: Option<String>,
    pub srt_path: Option<String>,
    pub slide_dir: Option<String>,
    /// Comma-separated tag string, e.g. `"rust,programming"`.
    pub tags: Option<String>,
    /// Short text excerpt used for full-text search.
    pub snippet: Option<String>,
    pub media_path: Option<String>,
    /// JSON metadata for downloaded media (container, bytes, requested kind).
    pub media_meta: Option<String>,
    pub media_action: Option<String>,
    /// SRT caption body kept on the record itself. Free-tier jobs never write
    /// timed-format files (those are a Pro export surface), so the reader's
    /// timestamp view and any later export derive from this stored copy.
    /// Pro rows may also keep it — detail reads prefer the file when present.
    pub srt_text: Option<String>,
    /// Seconds into the source where transcription stopped (Free-tier cap).
    /// `None` means the transcript covers the full source.
    pub truncated_at_s: Option<f64>,
}

// ---------------------------------------------------------------------------
// Schema DDL
// ---------------------------------------------------------------------------

const SCHEMA_DDL: &str = "
CREATE TABLE IF NOT EXISTS transcripts (
    id          TEXT    PRIMARY KEY NOT NULL,
    url         TEXT    NOT NULL,
    kind        TEXT    NOT NULL,
    title       TEXT,
    lang        TEXT,
    duration_s  REAL,
    created_at  TEXT    NOT NULL,
    txt_path    TEXT,
    md_path     TEXT,
    srt_path    TEXT,
    slide_dir   TEXT,
    tags        TEXT,
    snippet     TEXT,
    media_path  TEXT,
    media_meta  TEXT,
    media_action TEXT,
    srt_text    TEXT,
    truncated_at_s REAL
);
CREATE INDEX IF NOT EXISTS idx_transcripts_created_at
    ON transcripts (created_at DESC);
CREATE INDEX IF NOT EXISTS idx_transcripts_order_time
    ON transcripts (created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_transcripts_order_longest
    ON transcripts (COALESCE(duration_s, -1.0) DESC, created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_transcripts_order_shortest
    ON transcripts (COALESCE(duration_s, 1e308) ASC, created_at DESC, id DESC);
";

/// Sort order of a paged listing. `Oldest` walks the time index backwards;
/// `Longest` and `Shortest` order by duration with unknown durations last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryOrder {
    Newest,
    Oldest,
    Longest,
    Shortest,
}

/// SQL rank expression per order. Must match the index expressions in
/// `SCHEMA_DDL` textually so the planner can use them.
const LONGEST_RANK: &str = "COALESCE(duration_s, -1.0)";
const SHORTEST_RANK: &str = "COALESCE(duration_s, 1e308)";
/// Rank of an unknown duration, as the SQL above computes it.
const LONGEST_UNKNOWN_RANK: f64 = -1.0;
const SHORTEST_UNKNOWN_RANK: f64 = 1e308;

/// Keyset position of the last row a page returned: the rank (duration key;
/// ignored for the time orders), then `created_at` and `id`.
#[derive(Debug, Clone, PartialEq)]
pub struct PageKey {
    pub rank: f64,
    pub created_at: String,
    pub id: String,
}

impl HistoryOrder {
    /// The keyset position of `rec` under this order.
    pub fn key_of(self, rec: &HistoryRecord) -> PageKey {
        let rank = match self {
            HistoryOrder::Newest | HistoryOrder::Oldest => 0.0,
            HistoryOrder::Longest => rec.duration_s.unwrap_or(LONGEST_UNKNOWN_RANK),
            HistoryOrder::Shortest => rec.duration_s.unwrap_or(SHORTEST_UNKNOWN_RANK),
        };
        PageKey {
            rank,
            created_at: rec.created_at.clone(),
            id: rec.id.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// The history/search index.  Wraps a single SQLite connection.
pub struct Store(Connection);

impl Store {
    /// Open (or create) the store at `path`.
    ///
    /// Creates the schema automatically on first open (idempotent — `IF NOT EXISTS` guards).
    pub fn open(path: &Path) -> Result<Store> {
        let mut options = DbOptions::new(path);
        // Files remain canonical; this rebuildable index keeps NORMAL durability.
        options.durability = DurabilityProfile::Normal;
        // Preserve existing history.db and sidecar permissions.
        options.owner_only = false;
        let conn = options.open_writer().map_err(db_error_to_sql_error)?;
        apply_migrations(&conn, Some(path))?;
        Ok(Store(conn))
    }

    /// Open an existing store at `path` read-only (`rightkit_db::open_read_only`): never creates the
    /// file, never writes (no schema bootstrap, migration, journal change or checkpoint), so
    /// reading a library that must stay untouched (the source of a history merge) cannot alter it.
    /// The schema must already be current.
    pub fn open_read_only(path: &Path) -> Result<Store> {
        let conn = rightkit_db::open_read_only(path).map_err(db_error_to_sql_error)?;
        Ok(Store(conn))
    }

    /// Open an in-memory store.  Intended for tests and ephemeral use.
    /// Uses SQLite's default in-memory pragmas; no file bootstrap is applied.
    ///
    /// # Panics
    /// Panics if SQLite fails to open an in-memory database (should never happen
    /// in practice).
    pub fn open_in_memory() -> Store {
        let conn =
            rightkit_db::open_in_memory().expect("rightkit-db failed to open in-memory database");
        apply_migrations(&conn, None).expect("failed to apply migrations to in-memory database");
        Store(conn)
    }

    /// Schema version recorded by rightkit-db's migrator (`PRAGMA user_version`).
    pub fn schema_version(&self) -> Result<i64> {
        migration_version(&self.0, &VersionStorage::UserVersion).map_err(db_error_to_sql_error)
    }

    /// Insert or replace a record (keyed on `id`).
    pub fn upsert(&self, rec: &HistoryRecord) -> Result<()> {
        self.0.execute(
            "INSERT OR REPLACE INTO transcripts
             (id, url, kind, title, lang, duration_s, created_at,
              txt_path, md_path, srt_path, slide_dir, tags, snippet,
              media_path, media_meta, media_action, srt_text, truncated_at_s)
             VALUES
             (:id, :url, :kind, :title, :lang, :duration_s, :created_at,
              :txt_path, :md_path, :srt_path, :slide_dir, :tags, :snippet,
              :media_path, :media_meta, :media_action, :srt_text, :truncated_at_s)",
            named_params! {
                ":id":          &rec.id,
                ":url":         &rec.url,
                ":kind":        &rec.kind,
                ":title":       &rec.title,
                ":lang":        &rec.lang,
                ":duration_s":  &rec.duration_s,
                ":created_at":  &rec.created_at,
                ":txt_path":    &rec.txt_path,
                ":md_path":     &rec.md_path,
                ":srt_path":    &rec.srt_path,
                ":slide_dir":   &rec.slide_dir,
                ":tags":        &rec.tags,
                ":snippet":     &rec.snippet,
                ":media_path":  &rec.media_path,
                ":media_meta":  &rec.media_meta,
                ":media_action": &rec.media_action,
                ":srt_text":    &rec.srt_text,
                ":truncated_at_s": &rec.truncated_at_s,
            },
        )?;
        Ok(())
    }

    /// Fetch a single record by `id`, or `None` if it does not exist.
    pub fn get(&self, id: &str) -> Result<Option<HistoryRecord>> {
        let mut stmt = self.0.prepare(
            "SELECT id, url, kind, title, lang, duration_s, created_at,
                    txt_path, md_path, srt_path, slide_dir, tags, snippet,
                    media_path, media_meta, media_action, srt_text, truncated_at_s
             FROM transcripts WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_record(row)?)),
            None => Ok(None),
        }
    }

    /// Delete a single record by `id`.
    ///
    /// Returns `true` when a row was removed, `false` when the id was not in
    /// the index.
    pub fn delete(&self, id: &str) -> Result<bool> {
        let rows = self
            .0
            .execute("DELETE FROM transcripts WHERE id = ?1", params![id])?;
        Ok(rows > 0)
    }

    /// Copy rows from another on-disk store into this one, by primary key
    /// (`id`). This is an upsert: rows present in both stores end up in the
    /// destination as written by the destination's `upsert` (the destination
    /// row wins on id collision). The source store is opened read-only and
    /// never modified; rows that already exist in the destination are not
    /// counted as merged.
    ///
    /// Used by `crates/engine/src/history::merge_history` to carry the prior
    /// library forward when the user changes `output_dir`. Since
    /// `HistoryRecord` rows represent immutable past-job facts, "destination
    /// wins" is fine: any pre-existing row in the new store is the more
    /// recent fact (the new-dir was the live library between switches).
    ///
    /// Returns the number of rows actually inserted. A row already present in
    /// the destination is upserted in place (and counted as a no-op for the
    /// "merged" tally because the source bytes never reached the destination
    /// — caller-visible "merged" count is what was newly written).
    ///
    /// # Errors
    /// Returns the rusqlite error from opening or reading the source, or
    /// from upserting into the destination.
    pub fn merge_from_path(&self, source_path: &Path) -> Result<usize> {
        let source = Store::open(source_path)?;
        self.merge_from_store(&source)
    }

    /// Variant of `merge_from_path` that takes an already-opened `Store` for
    /// the source. Useful when the caller has the source open for some
    /// other reason (testing, multi-step migrations).
    pub fn merge_from_store(&self, source: &Store) -> Result<usize> {
        self.merge_from_store_with(source, |_| {})
    }

    /// Transform-merge: like `merge_from_store`, but `transform` rewrites
    /// each source row before the destination-wins insert. The history layer
    /// uses this to rebase absolute artifact paths when the old output dir's
    /// files are copied into the new one — the inserted rows must point at
    /// the copies, not at originals the user may later delete.
    pub fn merge_from_store_with(
        &self,
        source: &Store,
        mut transform: impl FnMut(&mut HistoryRecord),
    ) -> Result<usize> {
        let rows = source.page(None, usize::MAX)?;
        let mut inserted = 0usize;
        for row in &rows {
            // Destination wins: if the row already exists in the
            // destination, skip it entirely — do NOT upsert, because
            // `upsert` uses `INSERT OR REPLACE` semantics and would
            // clobber the destination row's fields with the source's.
            if self.get(&row.id)?.is_some() {
                continue;
            }
            let mut row = row.clone();
            transform(&mut row);
            self.upsert(&row)?;
            inserted += 1;
        }
        Ok(inserted)
    }

    /// Number of stored transcripts, from the index without reading rows.
    pub fn count(&self) -> Result<u64> {
        let n: i64 = self
            .0
            .query_row("SELECT COUNT(*) FROM transcripts", [], |row| row.get(0))?;
        Ok(n.max(0) as u64)
    }

    /// Keyset-paged listing in `order`. Rows strictly after `after` in that
    /// order are returned, at most `limit`; `after` is the [`PageKey`] of the
    /// last row already seen. Every order has a matching index, so a page reads
    /// only its own rows.
    pub fn page_ordered(
        &self,
        order: HistoryOrder,
        after: Option<&PageKey>,
        limit: usize,
    ) -> Result<Vec<HistoryRecord>> {
        let cap = limit as i64;
        const COLUMNS: &str = "SELECT id, url, kind, title, lang, duration_s, created_at,
                    txt_path, md_path, srt_path, slide_dir, tags, snippet,
                    media_path, media_meta, media_action, srt_text, truncated_at_s
             FROM transcripts";
        let sql = match (order, after.is_some()) {
            (HistoryOrder::Newest, false) => {
                format!("{COLUMNS} ORDER BY created_at DESC, id DESC LIMIT ?1")
            }
            (HistoryOrder::Newest, true) => format!(
                "{COLUMNS} WHERE created_at < ?1 OR (created_at = ?1 AND id < ?2)
                 ORDER BY created_at DESC, id DESC LIMIT ?3"
            ),
            (HistoryOrder::Oldest, false) => {
                format!("{COLUMNS} ORDER BY created_at ASC, id ASC LIMIT ?1")
            }
            (HistoryOrder::Oldest, true) => format!(
                "{COLUMNS} WHERE created_at > ?1 OR (created_at = ?1 AND id > ?2)
                 ORDER BY created_at ASC, id ASC LIMIT ?3"
            ),
            (HistoryOrder::Longest, false) => {
                format!("{COLUMNS} ORDER BY {LONGEST_RANK} DESC, created_at DESC, id DESC LIMIT ?1")
            }
            (HistoryOrder::Longest, true) => format!(
                "{COLUMNS} WHERE {LONGEST_RANK} < ?1
                    OR ({LONGEST_RANK} = ?1 AND (created_at < ?2 OR (created_at = ?2 AND id < ?3)))
                 ORDER BY {LONGEST_RANK} DESC, created_at DESC, id DESC LIMIT ?4"
            ),
            (HistoryOrder::Shortest, false) => {
                format!("{COLUMNS} ORDER BY {SHORTEST_RANK} ASC, created_at DESC, id DESC LIMIT ?1")
            }
            (HistoryOrder::Shortest, true) => format!(
                "{COLUMNS} WHERE {SHORTEST_RANK} > ?1
                    OR ({SHORTEST_RANK} = ?1 AND (created_at < ?2 OR (created_at = ?2 AND id < ?3)))
                 ORDER BY {SHORTEST_RANK} ASC, created_at DESC, id DESC LIMIT ?4"
            ),
        };
        let mut stmt = self.0.prepare_cached(&sql)?;
        let rows = match (order, after) {
            (_, None) => stmt.query_map(params![cap], row_to_record)?,
            (HistoryOrder::Newest, Some(k)) | (HistoryOrder::Oldest, Some(k)) => {
                stmt.query_map(params![k.created_at, k.id, cap], row_to_record)?
            }
            (HistoryOrder::Longest, Some(k)) | (HistoryOrder::Shortest, Some(k)) => {
                stmt.query_map(params![k.rank, k.created_at, k.id, cap], row_to_record)?
            }
        };
        rows.collect()
    }

    /// Keyset-paged listing, newest-first with a stable `(created_at, id)`
    /// order. `after` is the `(created_at, id)` of the last row already seen;
    /// rows strictly after it in that order are returned, at most `limit`.
    pub fn page(&self, after: Option<(&str, &str)>, limit: usize) -> Result<Vec<HistoryRecord>> {
        let cap = limit as i64;
        match after {
            None => {
                let mut stmt = self.0.prepare(
                    "SELECT id, url, kind, title, lang, duration_s, created_at,
                            txt_path, md_path, srt_path, slide_dir, tags, snippet,
                            media_path, media_meta, media_action, srt_text,
                            truncated_at_s
                     FROM transcripts
                     ORDER BY created_at DESC, id DESC
                     LIMIT ?1",
                )?;
                let rows = stmt.query_map(params![cap], row_to_record)?;
                rows.collect()
            }
            Some((created_at, id)) => {
                let mut stmt = self.0.prepare(
                    "SELECT id, url, kind, title, lang, duration_s, created_at,
                            txt_path, md_path, srt_path, slide_dir, tags, snippet,
                            media_path, media_meta, media_action, srt_text,
                            truncated_at_s
                     FROM transcripts
                     WHERE created_at < ?1 OR (created_at = ?1 AND id < ?2)
                     ORDER BY created_at DESC, id DESC
                     LIMIT ?3",
                )?;
                let rows = stmt.query_map(params![created_at, id, cap], row_to_record)?;
                rows.collect()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn db_error_to_sql_error(error: DbError) -> SqlError {
    // Preserve the store's rusqlite Result API and original SQLite errors.
    match error {
        DbError::Sqlite(error) => error,
        error => SqlError::ToSqlConversionFailure(Box::new(error)),
    }
}

/// Baseline (version 1): the `transcripts` table, its indexes, and every column added since.
/// Idempotent, so a database that predates versioning adopts it safely.
fn baseline(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA_DDL)?;
    for (column, ddl_type) in [
        ("md_path", "TEXT"),
        ("slide_dir", "TEXT"),
        ("tags", "TEXT"),
        ("media_path", "TEXT"),
        ("media_meta", "TEXT"),
        ("media_action", "TEXT"),
        ("srt_text", "TEXT"),
        ("truncated_at_s", "REAL"),
    ] {
        add_column_if_missing(conn, "transcripts", column, ddl_type)
            .map_err(db_error_to_sql_error)?;
    }
    Ok(())
}

/// Schema version this build writes. Append a `Migration` for the next schema change.
pub const CURRENT_SCHEMA_VERSION: i64 = 1;

const MIGRATIONS: &[Migration] = &[Migration::code(1, "baseline", baseline)];

/// Run pending migrations through rightkit-db. A database written by a newer build is
/// opened as-is, never lowered.
fn apply_migrations(conn: &Connection, path: Option<&Path>) -> Result<()> {
    let mut options = MigrateOptions::default();
    options.forward_compatible = Some(|_| true);
    migrate(conn, path, MIGRATIONS, &options)
        .map(|_| ())
        .map_err(db_error_to_sql_error)
}

fn row_to_record(row: &Row<'_>) -> Result<HistoryRecord> {
    Ok(HistoryRecord {
        id: row.get(0)?,
        url: row.get(1)?,
        kind: row.get(2)?,
        title: row.get(3)?,
        lang: row.get(4)?,
        duration_s: row.get(5)?,
        created_at: row.get(6)?,
        txt_path: row.get(7)?,
        md_path: row.get(8)?,
        srt_path: row.get(9)?,
        slide_dir: row.get(10)?,
        tags: row.get(11)?,
        snippet: row.get(12)?,
        media_path: row.get(13)?,
        media_meta: row.get(14)?,
        media_action: row.get(15)?,
        srt_text: row.get(16)?,
        truncated_at_s: row.get(17)?,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod order_tests {
    use super::*;

    fn rec(id: &str, created_at: &str, duration_s: Option<f64>) -> HistoryRecord {
        HistoryRecord {
            id: id.into(),
            url: format!("https://example.com/{id}"),
            kind: "youtube".into(),
            title: None,
            lang: None,
            duration_s,
            created_at: created_at.into(),
            txt_path: None,
            md_path: None,
            srt_path: None,
            slide_dir: None,
            tags: None,
            snippet: None,
            media_path: None,
            media_meta: None,
            media_action: None,
            srt_text: None,
            truncated_at_s: None,
        }
    }

    /// Four rows; "d" and "a" share a duration, "b" has none.
    fn seeded() -> Store {
        let store = Store::open_in_memory();
        for r in [
            rec("a", "2026-01-01", Some(60.0)),
            rec("b", "2026-01-03", None),
            rec("c", "2026-01-02", Some(10.0)),
            rec("d", "2026-01-04", Some(60.0)),
        ] {
            store.upsert(&r).unwrap();
        }
        store
    }

    /// Walks every page of `order` with `limit` rows each and returns the ids.
    fn walk(store: &Store, order: HistoryOrder, limit: usize) -> Vec<String> {
        let mut seen = Vec::new();
        let mut after: Option<PageKey> = None;
        loop {
            let page = store.page_ordered(order, after.as_ref(), limit).unwrap();
            let done = page.len() < limit;
            if let Some(last) = page.last() {
                after = Some(order.key_of(last));
            }
            seen.extend(page.into_iter().map(|r| r.id));
            if done {
                return seen;
            }
        }
    }

    #[test]
    fn count_reads_every_stored_row() {
        assert_eq!(seeded().count().unwrap(), 4);
        assert_eq!(Store::open_in_memory().count().unwrap(), 0);
    }

    #[test]
    fn each_order_pages_without_gaps_or_repeats() {
        let store = seeded();
        assert_eq!(walk(&store, HistoryOrder::Newest, 1), ["d", "b", "c", "a"]);
        assert_eq!(walk(&store, HistoryOrder::Oldest, 3), ["a", "c", "b", "d"]);
        assert_eq!(walk(&store, HistoryOrder::Longest, 2), ["d", "a", "c", "b"]);
        assert_eq!(
            walk(&store, HistoryOrder::Shortest, 1),
            ["c", "d", "a", "b"]
        );
    }

    #[test]
    fn unknown_durations_sort_last_in_both_directions() {
        let store = seeded();
        let longest = store.page_ordered(HistoryOrder::Longest, None, 10).unwrap();
        assert_eq!(longest.last().unwrap().id, "b");
        let shortest = store
            .page_ordered(HistoryOrder::Shortest, None, 10)
            .unwrap();
        assert_eq!(shortest.last().unwrap().id, "b");
    }

    #[test]
    fn order_queries_use_their_indexes() {
        let store = seeded();
        for (sql, index) in [
            (
                "SELECT id FROM transcripts ORDER BY COALESCE(duration_s, -1.0) DESC, created_at DESC, id DESC LIMIT 5",
                "idx_transcripts_order_longest",
            ),
            (
                "SELECT id FROM transcripts ORDER BY COALESCE(duration_s, 1e308) ASC, created_at DESC, id DESC LIMIT 5",
                "idx_transcripts_order_shortest",
            ),
        ] {
            let plan: String = {
                let mut stmt = store.0.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
                let details: Vec<String> = stmt
                    .query_map([], |row| row.get::<_, String>(3))
                    .unwrap()
                    .map(|d| d.unwrap())
                    .collect();
                details.join(" | ")
            };
            assert!(plan.contains(index), "plan did not use {index}: {plan}");
        }
    }
}

#[cfg(test)]
mod schema_version_tests {
    use super::*;

    fn rec(id: &str) -> HistoryRecord {
        HistoryRecord {
            id: id.into(),
            url: "https://example.com/a".into(),
            kind: "youtube".into(),
            title: None,
            lang: None,
            duration_s: None,
            created_at: "1".into(),
            txt_path: None,
            md_path: None,
            srt_path: None,
            slide_dir: None,
            tags: None,
            snippet: None,
            media_path: None,
            media_meta: None,
            media_action: None,
            srt_text: None,
            truncated_at_s: None,
        }
    }

    #[test]
    fn fresh_store_records_the_baseline_version() {
        let store = Store::open_in_memory();
        assert_eq!(store.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn migration_is_idempotent() {
        let store = Store::open_in_memory();
        apply_migrations(&store.0, None).unwrap();
        apply_migrations(&store.0, None).unwrap();
        assert_eq!(store.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn unversioned_database_is_stamped_and_keeps_rows() {
        let conn = rightkit_db::open_in_memory().unwrap();
        baseline(&conn).unwrap();
        let store = Store(conn);
        store.upsert(&rec("a")).unwrap();
        apply_migrations(&store.0, None).unwrap();
        assert_eq!(store.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
        assert!(store.get("a").unwrap().is_some());
    }

    #[test]
    fn newer_stored_version_opens_and_is_never_lowered() {
        let store = Store::open_in_memory();
        store.0.pragma_update(None, "user_version", 99).unwrap();
        apply_migrations(&store.0, None).unwrap();
        assert_eq!(store.schema_version().unwrap(), 99);
    }
}
