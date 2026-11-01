//! Library search (PENDING 89, G14): a full-text index over library assets, queried with
//! rightkit-search's query language.
//!
//! Indexed per asset: the prompt, model, provider and kind of the job that made it, plus project and
//! brand when a request carries them, are free text in an FTS5 table. The three asset facts (origin,
//! evidence, rights status) and the scope ids (venture, channel, account) recorded in `asset_facts`
//! are operator fields only (`origin:generated`, `venture:acme`), so a word like "generated" in a
//! query is never matched against facts. Not indexed yet: transcript, OCR, identity and embeddings;
//! every search reports them as `not_indexed`.
//!
//! Query language and SQL: rightkit-search owns both. `Schema::parse` turns the user's text into a
//! `Query` (words are prefix terms, "phrases", `-negation`, `a OR b`, `{a b}`, `(group)`, and
//! `operator:value`), and `Schema::compile` turns it into a parameterised WHERE body whose free text
//! is an `IN (SELECT rowid ... MATCH ?)` over the FTS table. This file only describes the schema,
//! ANDs the structured `filters` input onto the compiled predicate, and adds three limits the crate
//! does not provide: overall query length, no control characters, and "at least one constraint" (the
//! crate silently drops punctuation-only terms, so `!!!` would otherwise match every asset).
//!
//! Folding: the crate's SQL path does not fold text itself. Case and diacritic folding happen in the
//! FTS5 tokenizer (`unicode61 remove_diacritics 2`) on both the indexed text and the query terms, as
//! in the crate's own `tests/sqlite_e2e.rs`; indexed text is stored unmodified. The crate's `fold` is
//! used where it is load-bearing for this index: it is part of the index stamp (below).
//!
//! Index stamp: `library_search_meta` row `fold_version` records the tokenizer, the column layout
//! and the output of `rightkit_search::fold::fold` over a fixed probe string. When the stored stamp
//! differs from the current one (or is missing from a non-empty index) the whole index is rebuilt,
//! so a tokenizer or fold change rebuilds instead of silently mismatching. A fresh index records it.
//!
//! Paging: keyset (`cursor` / `next_cursor`), ordered newest asset first, then asset id. The crate has
//! no ranking, so hits are not relevance ordered.
//!
//! Storage: tables in the app database, created at open through `store::config()`'s `app_schema`
//! (idempotent, additive). `library_search_docs` holds one row per asset with the filterable columns;
//! `library_search_fts` is the FTS5 table, keyed by the docs row id. The index is derived data:
//! `library search` brings it up to date before it queries (adds assets it lacks, re-reads assets
//! whose facts arrived later, drops rows of deleted assets), `library rebuild-index` rebuilds it from
//! the asset records, and `asset delete` drops one asset's rows at once.

use std::path::{Path, PathBuf};

use crate::host::asset_facts::AssetFacts;
use rightkit_db::rusqlite::{self, params, params_from_iter, Connection, OptionalExtension};
use rightkit_search::snippet::{fts_snippet_expr, split_highlights};
use rightkit_search::{Binding, Compiled, Cursor, Keyset, Param, Schema};
use serde::Serialize;
use serde_json::{json, Value};

use crate::store::{self, db_err, je, Db};
use crate::{Context, GenError, GenResult};

/// Idempotent DDL run at every store open (see `store::config`). Keep the tokenizer string in step
/// with `TOKENIZER` and the column layout with `LAYOUT`.
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS library_search_docs (
  id INTEGER PRIMARY KEY,
  asset_id TEXT NOT NULL UNIQUE,
  job_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  provider TEXT NOT NULL,
  model TEXT NOT NULL,
  origin TEXT NOT NULL,
  evidence TEXT NOT NULL,
  rights_status TEXT NOT NULL,
  venture TEXT NOT NULL,
  channel TEXT NOT NULL,
  account TEXT NOT NULL,
  facts_present INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  indexed_at TEXT NOT NULL
);
CREATE VIRTUAL TABLE IF NOT EXISTS library_search_fts USING fts5(
  prompt, model, provider, kind, project, brand,
  tokenize = 'unicode61 remove_diacritics 2'
);
CREATE TABLE IF NOT EXISTS library_search_meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
";

const TOKENIZER: &str = "unicode61 remove_diacritics 2";
const LAYOUT: &str = "2";
const STAMP_KEY: &str = "fold_version";
/// Folded by the crate into the stamp: an uppercase dotted I, a word-final capital sigma, a precomposed
/// accent and a decomposed one, the cases `rightkit_search::fold` documents as its limits.
const FOLD_PROBE: &str = "İSTANBUL ΣΊΣΥΦΟΣ Café Cafe\u{301}";

/// What this slice does not index. Reported on every search so no caller reads a miss as absence.
pub const NOT_INDEXED: &[&str] = &["transcript", "ocr", "identity", "embedding"];

/// Closed vocabularies, as operator values (`origin:generated`) and as `filters` values. The crate's
/// `Binding::Flags` refuses a value outside the table as a compile error.
const ORIGIN_FLAGS: &[(&str, &str)] = &[
    ("unknown", "d.origin = 'unknown'"),
    ("generated", "d.origin = 'generated'"),
    ("captured", "d.origin = 'captured'"),
    ("acquired", "d.origin = 'acquired'"),
    ("supplied", "d.origin = 'supplied'"),
];
const EVIDENCE_FLAGS: &[(&str, &str)] = &[
    ("unknown", "d.evidence = 'unknown'"),
    ("authentic", "d.evidence = 'authentic'"),
    ("illustrative", "d.evidence = 'illustrative'"),
];
const RIGHTS_FLAGS: &[(&str, &str)] = &[
    ("unknown", "d.rights_status = 'unknown'"),
    ("unresolved", "d.rights_status = 'unresolved'"),
    ("cleared", "d.rights_status = 'cleared'"),
];

/// Filters a search accepts; each names one column of `library_search_docs`.
const FILTERS: &[&str] = &[
    "kind",
    "provider",
    "origin",
    "rights_status",
    "venture",
    "channel",
    "account",
];
const SEARCH_KEYS: &[&str] = &["query", "limit", "cursor", "filters"];
/// Limits the crate does not provide (see the module note).
const MAX_QUERY_CHARS: usize = 256;
const MAX_FILTER_CHARS: usize = 128;
const MAX_CURSOR_CHARS: usize = 512;

const SELECT_HITS: &str = "SELECT d.asset_id, d.job_id, a.path, d.kind, d.provider, d.model, f.facts_json, f.scope_json, d.id, d.created_at
           FROM library_search_docs d
           JOIN assets a ON a.asset_id = d.asset_id
           LEFT JOIN asset_facts f ON f.asset_id = d.asset_id";

fn invalid(message: impl Into<String>) -> GenError {
    GenError::new("invalid_input", message)
}

/// How the host's index maps onto rightkit-search: free text matches the FTS5 table (keyed by the
/// docs row id); operators map to docs columns.
fn schema() -> Schema {
    Schema::new()
        .fts("library_search_fts", "d.id")
        .bind("kind", Binding::Eq("d.kind"))
        .bind("provider", Binding::Eq("d.provider"))
        .bind("model", Binding::Eq("d.model"))
        .bind("origin", Binding::Flags(ORIGIN_FLAGS))
        .bind("evidence", Binding::Flags(EVIDENCE_FLAGS))
        .bind("rights", Binding::Flags(RIGHTS_FLAGS))
        .bind("venture", Binding::Eq("d.venture"))
        .bind("channel", Binding::Eq("d.channel"))
        .bind("account", Binding::Eq("d.account"))
}

/// Sort order of every page: newest asset first, then asset id (unique, never null).
fn keyset() -> Keyset {
    Keyset::new().desc("d.created_at").asc("d.asset_id")
}

/// Parse and compile the user's query with rightkit-search. Parse and compile errors carry the
/// crate's message as `invalid_input`; no query text reaches SQL or FTS5 except through the crate.
fn compile_query(text: &str) -> GenResult<Compiled> {
    if text.chars().count() > MAX_QUERY_CHARS {
        return Err(invalid(format!(
            "query is longer than {MAX_QUERY_CHARS} characters"
        )));
    }
    if text.chars().any(|c| c.is_control() && !c.is_whitespace()) {
        return Err(invalid("query must not contain control characters"));
    }
    let schema = schema();
    let query = schema
        .parse(text)
        .map_err(|e| invalid(format!("query could not be parsed: {e}")))?;
    let compiled = schema
        .compile(&query)
        .map_err(|e| invalid(format!("query could not be compiled: {e}")))?;
    let constrained = !query.ops.is_empty()
        || !query.op_or_groups.is_empty()
        || compiled.fts_match.is_some()
        || !query.fts_negated().is_empty();
    if !constrained {
        return Err(invalid("query needs at least one word, phrase or operator"));
    }
    Ok(compiled)
}

/// AND the structured filters onto a compiled predicate. Column names come from `FILTERS`; values
/// are bound after the predicate's own parameters, in placeholder order.
fn with_filters(mut compiled: Compiled, filters: &[(&'static str, String)]) -> Compiled {
    for (column, value) in filters {
        compiled.where_sql = format!("({}) AND d.{column} = ?", compiled.where_sql);
        compiled.params.push(Param::Text(value.clone()));
    }
    compiled
}

/// Reject keys the command does not know, by name, as the generate path does.
fn reject_unknown(input: &Value, accepted: &[&str]) -> GenResult<()> {
    let Some(map) = input.as_object() else {
        return Ok(());
    };
    for key in map.keys() {
        if !accepted.contains(&key.as_str()) {
            return Err(invalid(format!(
                "unknown key {key}; accepted keys: {}",
                accepted.join(", ")
            )));
        }
    }
    Ok(())
}

fn int_field(input: &Value, key: &str, default: i64, min: i64, max: i64) -> GenResult<i64> {
    match input.get(key).filter(|v| !v.is_null()) {
        None => Ok(default),
        Some(v) => v
            .as_i64()
            .filter(|n| (min..=max).contains(n))
            .ok_or_else(|| invalid(format!("{key} must be an integer from {min} to {max}"))),
    }
}

/// The optional `cursor` string from a previous result's `next_cursor`.
fn cursor_field(input: &Value) -> GenResult<Option<Cursor>> {
    let Some(v) = input.get("cursor").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let text = v
        .as_str()
        .filter(|s| !s.is_empty() && s.chars().count() <= MAX_CURSOR_CHARS)
        .ok_or_else(|| invalid("cursor must be the next_cursor string of a previous result"))?;
    Cursor::decode(text)
        .map(Some)
        .map_err(|e| invalid(format!("cursor: {e}")))
}

/// The `filters` object as (column, value) pairs. Unknown names and values outside the closed
/// origin and rights vocabularies are refused, so a typo is never an empty result.
fn filter_clauses(input: &Value) -> GenResult<Vec<(&'static str, String)>> {
    let Some(filters) = input.get("filters").filter(|v| !v.is_null()) else {
        return Ok(Vec::new());
    };
    let map = filters
        .as_object()
        .ok_or_else(|| invalid("filters must be an object"))?;
    let mut out = Vec::new();
    for (key, value) in map {
        let column = FILTERS
            .iter()
            .copied()
            .find(|c| *c == key.as_str())
            .ok_or_else(|| {
                invalid(format!(
                    "unknown filter {key}; accepted: {}",
                    FILTERS.join(", ")
                ))
            })?;
        let text = value
            .as_str()
            .filter(|s| !s.is_empty() && s.chars().count() <= MAX_FILTER_CHARS)
            .ok_or_else(|| invalid(format!("filter {key} must be a non-empty string")))?;
        let vocabulary: Option<&[(&str, &str)]> = match column {
            "origin" => Some(ORIGIN_FLAGS),
            "rights_status" => Some(RIGHTS_FLAGS),
            _ => None,
        };
        if let Some(allowed) = vocabulary {
            if !allowed.iter().any(|(name, _)| *name == text) {
                let names: Vec<&str> = allowed.iter().map(|(name, _)| *name).collect();
                return Err(invalid(format!(
                    "filter {key} must be one of: {}",
                    names.join(", ")
                )));
            }
        }
        out.push((column, text.to_string()));
    }
    Ok(out)
}

/// `library search` `{query, limit?, cursor?, filters?}` → `{query, total, limit, filters, hits,
/// next_cursor, index, not_indexed}`. `query` is rightkit-search syntax. Hits are newest asset first,
/// then asset id, so two processes over the same records return the same order; `next_cursor` is
/// null on the last page.
pub fn search_command(ctx: &Context, input: &Value) -> GenResult<Value> {
    reject_unknown(input, SEARCH_KEYS)?;
    let text = input
        .get("query")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("query must be a string"))?;
    let compiled = compile_query(text)?;
    let limit = int_field(input, "limit", 20, 1, 200)?;
    let after = cursor_field(input)?;
    let filters = filter_clauses(input)?;
    let compiled = with_filters(compiled, &filters);
    let paged = compiled
        .page(SELECT_HITS, &keyset(), after.as_ref(), limit as usize)
        .map_err(|e| invalid(format!("cursor: {e}")))?;

    let db = store::open(ctx)?;
    let refreshed = refresh(&db)?;
    let found = db
        .read(|c| {
            Ok(search_rows(
                c,
                &compiled,
                &paged.sql,
                &paged.params,
                limit as usize,
            ))
        })
        .map_err(je)?;
    let (indexed, total, rows, next_cursor) = found.map_err(sql_error)?;
    let library = ctx.library_dir();
    let hits: Vec<Value> = rows
        .into_iter()
        .map(|row| hit_value(&library, row))
        .collect();
    let filters_value = Value::Object(
        filters
            .iter()
            .map(|(column, value)| (column.to_string(), Value::String(value.clone())))
            .collect(),
    );
    Ok(json!({
        "query": text,
        "total": total,
        "limit": limit,
        "filters": filters_value,
        "hits": hits,
        "next_cursor": next_cursor,
        "index": {"assets": indexed, "refreshed": refreshed.changed, "rebuilt": refreshed.rebuilt},
        "not_indexed": NOT_INDEXED,
    }))
}

/// `library rebuild-index` → `{indexed}`: empty the index and index every asset record again.
pub fn rebuild_command(ctx: &Context, input: &Value) -> GenResult<Value> {
    reject_unknown(input, &[])?;
    let db = store::open(ctx)?;
    let indexed = rebuild(&db)?;
    Ok(json!({"indexed": indexed}))
}

/// The stamp a current index carries: layout, tokenizer, and the crate's folding of a fixed probe.
/// A change to any of them makes a stored index stale.
fn index_stamp() -> String {
    format!(
        "layout={LAYOUT};tokenizer={TOKENIZER};fold={}",
        rightkit_search::fold::fold(FOLD_PROBE)
    )
}

fn stored_stamp(c: &Connection) -> rusqlite::Result<Option<String>> {
    c.query_row(
        "SELECT value FROM library_search_meta WHERE key = ?1",
        [STAMP_KEY],
        |r| r.get(0),
    )
    .optional()
}

fn set_stamp(c: &Connection, stamp: &str) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO library_search_meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![STAMP_KEY, stamp],
    )?;
    Ok(())
}

fn doc_count(c: &Connection) -> rusqlite::Result<i64> {
    c.query_row("SELECT count(*) FROM library_search_docs", [], |r| r.get(0))
}

/// Result of `refresh`: rows changed, and whether the whole index was rebuilt because its stamp differed.
pub struct Refresh {
    pub changed: i64,
    pub rebuilt: bool,
}

/// Empty the index, index every asset that has a job record, and record the current stamp. Returns
/// the rows written.
pub fn rebuild(db: &Db) -> GenResult<i64> {
    db.write(|c| Ok(rebuild_in(c)?)).map_err(je)
}

fn rebuild_in(c: &Connection) -> rusqlite::Result<i64> {
    c.execute("DELETE FROM library_search_fts", [])?;
    c.execute("DELETE FROM library_search_docs", [])?;
    let ids = asset_ids(
        c,
        "SELECT a.asset_id FROM assets a JOIN jobs j ON j.job_id = a.job_id ORDER BY a.asset_id",
    )?;
    let mut indexed: i64 = 0;
    for id in &ids {
        if index_one(c, id)? {
            indexed += 1;
        }
    }
    set_stamp(c, &index_stamp())?;
    Ok(indexed)
}

/// Bring the index up to date with the asset records before a query. A stored stamp that differs from
/// the current one (or is absent from a non-empty index) rebuilds the whole index. Otherwise it adds
/// assets the index lacks or whose facts were not yet recorded when they were indexed, and drops rows
/// whose asset is gone (0 changes when the index was current, which costs one read). A fresh index
/// records the stamp.
pub fn refresh(db: &Db) -> GenResult<Refresh> {
    let want = index_stamp();
    let (stored, docs, behind_ids, orphan_ids) = db
        .read(|c| Ok((stored_stamp(c)?, doc_count(c)?, behind(c)?, orphaned(c)?)))
        .map_err(je)?;
    let stale = match &stored {
        Some(s) => *s != want,
        None => docs > 0,
    };
    if stale {
        let changed = db.write(|c| Ok(rebuild_in(c)?)).map_err(je)?;
        return Ok(Refresh {
            changed,
            rebuilt: true,
        });
    }
    if stored.is_some() && behind_ids.is_empty() && orphan_ids.is_empty() {
        return Ok(Refresh {
            changed: 0,
            rebuilt: false,
        });
    }
    let fresh = stored.is_none();
    let changed = db
        .write(|c| {
            if fresh {
                set_stamp(c, &want)?;
            }
            let gone = orphaned(c)?;
            for id in &gone {
                drop_asset(c, id)?;
            }
            let mut changed = gone.len() as i64;
            for id in behind(c)? {
                if index_one(c, &id)? {
                    changed += 1;
                }
            }
            Ok(changed)
        })
        .map_err(je)?;
    Ok(Refresh {
        changed,
        rebuilt: false,
    })
}

/// Drop one asset's index rows. Called by `asset delete` in the same transaction that removes its
/// asset and facts rows.
pub fn drop_asset(c: &Connection, asset_id: &str) -> rusqlite::Result<()> {
    c.execute(
        "DELETE FROM library_search_fts WHERE rowid IN (SELECT id FROM library_search_docs WHERE asset_id = ?1)",
        [asset_id],
    )?;
    c.execute(
        "DELETE FROM library_search_docs WHERE asset_id = ?1",
        [asset_id],
    )?;
    Ok(())
}

/// Assets with a job record that the index lacks, or indexed before their facts existed.
fn behind(c: &Connection) -> rusqlite::Result<Vec<String>> {
    asset_ids(
        c,
        "SELECT a.asset_id
           FROM assets a
           JOIN jobs j ON j.job_id = a.job_id
           LEFT JOIN library_search_docs d ON d.asset_id = a.asset_id
           LEFT JOIN asset_facts f ON f.asset_id = a.asset_id
          WHERE d.asset_id IS NULL OR (d.facts_present = 0 AND f.asset_id IS NOT NULL)
          ORDER BY a.asset_id",
    )
}

/// Index rows whose asset (or its job) no longer exists.
fn orphaned(c: &Connection) -> rusqlite::Result<Vec<String>> {
    asset_ids(
        c,
        "SELECT d.asset_id
           FROM library_search_docs d
          WHERE NOT EXISTS (
            SELECT 1 FROM assets a JOIN jobs j ON j.job_id = a.job_id WHERE a.asset_id = d.asset_id
          )
          ORDER BY d.asset_id",
    )
}

fn asset_ids(c: &Connection, sql: &str) -> rusqlite::Result<Vec<String>> {
    let mut st = c.prepare(sql)?;
    let ids = st
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ids)
}

struct Source {
    job_id: String,
    kind: String,
    provider: String,
    model: String,
    request_json: String,
    created_at: String,
    facts_json: Option<String>,
    scope_json: Option<String>,
}

/// Replace one asset's index rows from its records. Returns false when the asset has no job record
/// (its rows are then left absent).
fn index_one(c: &Connection, asset_id: &str) -> rusqlite::Result<bool> {
    drop_asset(c, asset_id)?;
    let source = c
        .query_row(
            "SELECT j.job_id, j.kind, j.provider, j.model, j.request_json, a.created_at, f.facts_json, f.scope_json
               FROM assets a
               JOIN jobs j ON j.job_id = a.job_id
               LEFT JOIN asset_facts f ON f.asset_id = a.asset_id
              WHERE a.asset_id = ?1",
            [asset_id],
            |r| {
                Ok(Source {
                    job_id: r.get(0)?,
                    kind: r.get(1)?,
                    provider: r.get(2)?,
                    model: r.get(3)?,
                    request_json: r.get(4)?,
                    created_at: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    facts_json: r.get(6)?,
                    scope_json: r.get(7)?,
                })
            },
        )
        .optional()?;
    let Some(source) = source else {
        return Ok(false);
    };

    let request: Value = serde_json::from_str(&source.request_json).unwrap_or(Value::Null);
    let parsed = source
        .facts_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .map(|v| AssetFacts::from_value(&v));
    let facts_present: i64 = i64::from(parsed.is_some());
    let facts = parsed.unwrap_or_else(AssetFacts::unknown);
    let scope: Value = source
        .scope_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Null);
    let scope_text = |key: &str| {
        scope
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let (venture, channel, account) = (
        scope_text("venture"),
        scope_text("channel"),
        scope_text("account"),
    );
    let prompt = request_text(&request, "prompt");
    let project = request_text(&request, "project");
    let brand = request_text(&request, "brand");
    let origin = label(&facts.origin.kind);
    let evidence = label(&facts.evidence);
    let rights = label(&facts.rights.status);

    c.execute(
        "INSERT INTO library_search_docs (asset_id, job_id, kind, provider, model, origin, evidence, rights_status, venture, channel, account, facts_present, created_at, indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            asset_id, source.job_id, source.kind, source.provider, source.model, origin, evidence, rights,
            venture, channel, account, facts_present, source.created_at, store::now()
        ],
    )?;
    let id = c.last_insert_rowid();
    c.execute(
        "INSERT INTO library_search_fts (rowid, prompt, model, provider, kind, project, brand)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id,
            prompt,
            source.model,
            source.provider,
            source.kind,
            project,
            brand
        ],
    )?;
    Ok(true)
}

/// A request field read from the top level, or from `params`, as text ("" when absent).
fn request_text(request: &Value, key: &str) -> String {
    request
        .get(key)
        .and_then(Value::as_str)
        .or_else(|| {
            request
                .get("params")
                .and_then(|p| p.get(key))
                .and_then(Value::as_str)
        })
        .unwrap_or("")
        .to_string()
}

/// A closed enum's serde name ("generated", "unresolved", ...).
fn label<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_string())
}

struct RawHit {
    id: i64,
    created_at: String,
    asset_id: String,
    job_id: String,
    stored_path: String,
    kind: String,
    provider: String,
    model: String,
    snippet: String,
    facts_json: Option<String>,
    scope_json: Option<String>,
}

/// Run the count and one keyset page. The SQL comes from rightkit-search (`Compiled::page`); only
/// values are bound. Returns (index size, total matches, kept hits, cursor of the next page).
fn search_rows(
    c: &Connection,
    compiled: &Compiled,
    page_sql: &str,
    page_params: &[Param],
    limit: usize,
) -> rusqlite::Result<(i64, i64, Vec<RawHit>, Option<String>)> {
    let total: i64 = c.query_row(
        &compiled.select("SELECT count(*) FROM library_search_docs d"),
        params_from_iter(compiled.params.iter()),
        |r| r.get(0),
    )?;
    let indexed = doc_count(c)?;
    let mut st = c.prepare(page_sql)?;
    let mut hits = st
        .query_map(params_from_iter(page_params.iter()), |r| {
            Ok(RawHit {
                asset_id: r.get(0)?,
                job_id: r.get(1)?,
                stored_path: r.get(2)?,
                kind: r.get(3)?,
                provider: r.get(4)?,
                model: r.get(5)?,
                facts_json: r.get(6)?,
                scope_json: r.get(7)?,
                id: r.get(8)?,
                created_at: r.get(9)?,
                snippet: String::new(),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    // The statement fetched limit + 1 rows: a row beyond the page means another page exists.
    let more = hits.len() > limit;
    hits.truncate(limit);
    let next = if more {
        hits.last().map(|h| {
            Cursor::new(vec![
                Param::Text(h.created_at.clone()),
                Param::Text(h.asset_id.clone()),
            ])
            .encode()
        })
    } else {
        None
    };
    for hit in &mut hits {
        hit.snippet = snippet_for(c, compiled.fts_match.as_deref(), hit.id)?;
    }
    Ok((indexed, total, hits, next))
}

/// The best-matching indexed field of one hit with matches in `[brackets]`, from FTS5's `snippet()`
/// through the crate's expression helper. Without positive free text (an operator-only query) it is
/// the start of the prompt.
fn snippet_for(c: &Connection, fts_match: Option<&str>, id: i64) -> rusqlite::Result<String> {
    if let Some(expression) = fts_match {
        let sql = format!(
            "SELECT {} FROM library_search_fts WHERE library_search_fts MATCH ?1 AND rowid = ?2",
            fts_snippet_expr("library_search_fts", 12)
        );
        let marked: Option<String> = c
            .query_row(&sql, params![expression, id], |r| r.get(0))
            .optional()?;
        if let Some(marked) = marked {
            return Ok(split_highlights(&marked)
                .into_iter()
                .map(|span| {
                    if span.matched {
                        format!("[{}]", span.text)
                    } else {
                        span.text
                    }
                })
                .collect());
        }
    }
    let prompt: Option<String> = c
        .query_row(
            "SELECT prompt FROM library_search_fts WHERE rowid = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    let prompt = prompt.unwrap_or_default();
    let head: String = prompt.chars().take(120).collect();
    Ok(if head.chars().count() < prompt.chars().count() {
        format!("{head}…")
    } else {
        head
    })
}

/// An FTS5 parse failure is the caller's query, so it is `invalid_input`; any other SQL error is
/// a database fault.
fn sql_error(e: rusqlite::Error) -> GenError {
    let text = e.to_string();
    if text.contains("fts5") {
        invalid(format!("query could not be parsed: {text}"))
    } else {
        db_err(e)
    }
}

/// One hit as the caller sees it. The path follows `asset resolve`: the current library folder
/// first, then the recorded path, and `exists` says whether either is a file.
fn hit_value(library: &Path, row: RawHit) -> Value {
    let stored = PathBuf::from(&row.stored_path);
    let name = stored.file_name().map(|n| n.to_owned()).unwrap_or_default();
    let current = library.join(&row.asset_id).join(&name);
    let (path, exists) = if current.is_file() {
        (current, true)
    } else if stored.is_file() {
        (stored, true)
    } else {
        (current, false)
    };
    let facts = row
        .facts_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .map(|v| AssetFacts::from_value(&v))
        .unwrap_or_else(AssetFacts::unknown);
    let scope = row
        .scope_json
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .unwrap_or(Value::Null);
    json!({
        "asset_id": row.asset_id,
        "job_id": row.job_id,
        "path": path.to_string_lossy(),
        "exists": exists,
        "kind": row.kind,
        "provider": row.provider,
        "model": row.model,
        "prompt_snippet": row.snippet,
        "facts": facts.to_value(),
        "scope": scope,
    })
}
