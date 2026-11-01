//! Ledger transcript lane: authorized ChatGPT and Codex history, searchable by exact words.
//!
//! Scope of this lane (upstream #431; plan: docs/architecture/2026-10-21-transcript-search-ledger-embeddings.md):
//!
//! * **Off by default.** Nothing is read, indexed or kept unless the source manifest
//!   (`ledger-transcript-sources.json` in the Membrane state root, written by the host app when the user
//!   turns the setting on) exists, says `enabled: true`, and names the source. Absent or disabled,
//!   maintenance purges whatever this lane stored: the copy is a rebuildable projection, the
//!   provider files stay authoritative.
//! * **Authorized sources only.** Each manifest entry names one root, one provider (`codex`:
//!   rollout `.jsonl` files; `chatgpt`: an extracted data export folder with
//!   `conversations*.json`), one device and one scope. No scraping, no cloud capture, no
//!   discovery beyond the named roots; symlinks are never followed.
//! * **Incremental and idempotent.** Per-file cursors (consumed bytes plus prefix digest for
//!   append-only rollouts; whole-file digest for exports) make a rescan of unchanged files free and
//!   a rescan of changed ones resume. Messages are keyed by (scope, provider, conversation,
//!   message) and revised by text digest, so replay writes nothing, an edit is a new revision and
//!   the same message seen on a second device only adds a `seen` row. Equal text never merges
//!   distinct messages; branches stay distinct nodes.
//! * **Absence is not deletion.** A missing or unreadable source marks its files `missing` and
//!   keeps the history (invariant 23). Only an explicit erase deletes, and its tombstone lives in
//!   the durable catalog (like `erasure.rs`), so a rebuild of the index cannot bring it back.
//! * **Privacy.** Secret shapes are redacted and private reasoning, system instructions, account
//!   ids and absolute paths never reach a row (see `transcript_parse`). Scope is mandatory on
//!   every read: a hit can only come from a scope the caller names. Transcript text is evidence,
//!   never an instruction source, and is not offered to Pull as a candidate lane.
//! * **Read-time authorization.** Every search, expand and status re-reads the manifest at query
//!   time: the lane must be `enabled` and every scope asked for must be named by a manifest
//!   source, an explicit scope included. Switching the setting off therefore refuses reads at
//!   once; the stored copy is purged later by maintenance, never relied on to stop a read. A
//!   manifest that cannot be read refuses too.
//! * **This computer only.** Reads answer from this computer's own index, for a caller on this
//!   computer. No remote or cross-device search path exists and none may be added until an
//!   authenticated per-device history-read grant exists (a paired device's link does not grant
//!   history: snippets are transcript data leaving the device). Scopes name this computer's own
//!   sources (`codex-<device>`, `chatgpt`), never another computer's.
//! * **Lexical only.** Ledger-local FTS5 over message chunks; no vectors (#432 is separate).
use super::{index, limits::WorkBudget, transcript_parse as parse, LedgerDb};
use crate::catalog::ContextCatalog;
use rightkit_db::rusqlite::{self, params, Connection, OptionalExtension};
use rightkit_fs::digest::{sha256_bytes, sha256_file, Sha256Hasher};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const MANIFEST_SCHEMA: &str = "ledger.transcript-sources.v1";
pub const MANIFEST_FILE: &str = "ledger-transcript-sources.json";
const STORE_SCHEMA: &str = "ledger.transcripts.v1";
/// Most bytes of one rollout file read in one pass; the cursor resumes the rest next cycle.
const MAX_TAIL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EXPORT_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_FILES_PER_SOURCE: usize = 50_000;
const MAX_WALK_DEPTH: usize = 8;
const MAX_SOURCES: usize = 64;
const BATCH_CONVERSATIONS: usize = 100;
const MAX_TOMBSTONES: usize = 200_000;
pub const MAX_RESULTS: usize = 50;
pub const MAX_EXPAND: usize = 20;
const SNIPPET_TOKENS: i64 = 24;

const TX_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS ledger_tx_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS ledger_tx_sources(
  source_id TEXT PRIMARY KEY, device TEXT NOT NULL, provider TEXT NOT NULL, kind TEXT NOT NULL,
  scope TEXT NOT NULL, root_digest TEXT NOT NULL, last_scan_ms INTEGER NOT NULL,
  status TEXT NOT NULL, detail TEXT NOT NULL DEFAULT '', files_seen INTEGER NOT NULL DEFAULT 0,
  files_failed INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS ledger_tx_files(
  source_id TEXT NOT NULL, rel_path TEXT NOT NULL, size INTEGER NOT NULL, mtime_ms INTEGER NOT NULL,
  consumed_bytes INTEGER NOT NULL, lines INTEGER NOT NULL DEFAULT 0, prefix_sha256 TEXT NOT NULL,
  parser_version TEXT NOT NULL, conv_id TEXT, state TEXT NOT NULL, detail TEXT NOT NULL DEFAULT '',
  updated_ms INTEGER NOT NULL, PRIMARY KEY(source_id, rel_path));
CREATE TABLE IF NOT EXISTS ledger_tx_conversations(
  conv_key TEXT PRIMARY KEY, scope TEXT NOT NULL, provider TEXT NOT NULL, provider_conv_id TEXT NOT NULL,
  title TEXT NOT NULL DEFAULT '', created_ms INTEGER NOT NULL DEFAULT 0, updated_ms INTEGER NOT NULL DEFAULT 0,
  branch TEXT, workspace TEXT, parent_conv_id TEXT, current_msg_id TEXT, state TEXT NOT NULL DEFAULT 'active');
CREATE INDEX IF NOT EXISTS idx_ledger_tx_conv_scope ON ledger_tx_conversations(scope, updated_ms);
CREATE TABLE IF NOT EXISTS ledger_tx_messages(
  msg_key TEXT NOT NULL, revision TEXT NOT NULL, conv_key TEXT NOT NULL, provider_msg_id TEXT NOT NULL,
  parent_msg_id TEXT, role TEXT NOT NULL, kind TEXT NOT NULL, ordinal INTEGER NOT NULL,
  created_ms INTEGER NOT NULL, text TEXT NOT NULL, text_sha256 TEXT NOT NULL, truncated INTEGER NOT NULL DEFAULT 0,
  on_current_path INTEGER NOT NULL DEFAULT 1, state TEXT NOT NULL, first_seen_ms INTEGER NOT NULL,
  PRIMARY KEY(msg_key, revision));
CREATE INDEX IF NOT EXISTS idx_ledger_tx_msg_conv ON ledger_tx_messages(conv_key, ordinal);
CREATE TABLE IF NOT EXISTS ledger_tx_seen(
  msg_key TEXT NOT NULL, revision TEXT NOT NULL, device TEXT NOT NULL, source_id TEXT NOT NULL,
  rel_path TEXT NOT NULL, byte_start INTEGER NOT NULL, byte_end INTEGER NOT NULL, raw_sha256 TEXT NOT NULL,
  locator TEXT NOT NULL, seen_ms INTEGER NOT NULL, PRIMARY KEY(msg_key, revision, device, source_id));
CREATE INDEX IF NOT EXISTS idx_ledger_tx_seen_source ON ledger_tx_seen(source_id);
CREATE TABLE IF NOT EXISTS ledger_tx_chunks(
  chunk_id INTEGER PRIMARY KEY, msg_key TEXT NOT NULL, revision TEXT NOT NULL, ix INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS idx_ledger_tx_chunks_msg ON ledger_tx_chunks(msg_key, revision);
CREATE VIRTUAL TABLE IF NOT EXISTS ledger_tx_fts USING fts5(body, tokenize='unicode61 remove_diacritics 2');
"#;
const TX_TABLES: &[&str] = &[
    "ledger_tx_seen",
    "ledger_tx_chunks",
    "ledger_tx_messages",
    "ledger_tx_conversations",
    "ledger_tx_files",
    "ledger_tx_sources",
];
/// Durable deny records, kept in the catalog beside the document exclusions.
const CATALOG_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS ledger_tx_tombstones(
  conv_key TEXT PRIMARY KEY, scope TEXT NOT NULL, erased_at_ms INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS ledger_tx_source_exclusions(
  source_id TEXT PRIMARY KEY, erased_at_ms INTEGER NOT NULL);";

// ---- manifest ----

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub id: String,
    /// The computer the history belongs to (`mac`, `dell`, ...); `account` for a provider export.
    pub device: String,
    /// `codex` or `chatgpt`.
    pub provider: String,
    /// Absolute folder: a Codex sessions folder (or a local mirror of another computer's), or an
    /// extracted ChatGPT export.
    pub root: String,
    /// Isolation label. Reads name the scopes they may see; the same conversation id in two scopes
    /// is two conversations.
    pub scope: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub sources: Vec<Source>,
}

pub fn manifest_path() -> PathBuf {
    crate::paths::state_root().join(MANIFEST_FILE)
}

fn label_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// `Ok(None)`: no manifest (the lane is off). `Err`: present but unusable, which is a typed
/// failure and never a reason to delete stored history.
pub fn load_manifest(path: &Path) -> Result<Option<Manifest>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("ledger_transcript_manifest_unreadable".into()),
    };
    if bytes.len() > 1024 * 1024 {
        return Err("ledger_transcript_manifest_too_large".into());
    }
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|_| "ledger_transcript_manifest_invalid")?;
    if manifest.schema != MANIFEST_SCHEMA {
        return Err("ledger_transcript_manifest_schema_unsupported".into());
    }
    if manifest.sources.len() > MAX_SOURCES {
        return Err("ledger_transcript_manifest_too_many_sources".into());
    }
    let mut ids = BTreeSet::new();
    for source in &manifest.sources {
        if !label_ok(&source.id)
            || !label_ok(&source.device)
            || !label_ok(&source.scope)
            || !ids.insert(source.id.as_str())
            || !matches!(source.provider.as_str(), "codex" | "chatgpt")
            || !Path::new(&source.root).is_absolute()
        {
            return Err("ledger_transcript_manifest_invalid".into());
        }
    }
    Ok(Some(manifest))
}

// ---- keys ----

fn conv_key(scope: &str, provider: &str, conv_id: &str) -> String {
    sha256_bytes(format!("{scope}\0{provider}\0{conv_id}").as_bytes())
}
fn msg_key(conv_key: &str, message_id: &str) -> String {
    sha256_bytes(format!("{conv_key}\0{message_id}").as_bytes())
}
fn now_ms() -> i64 {
    crate::time::now_millis() as i64
}
fn is_hex64(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

// ---- schema, purge ----

pub fn ensure_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(TX_SCHEMA).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT OR IGNORE INTO ledger_tx_meta VALUES ('schema', ?1)",
        [STORE_SCHEMA],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn schema_present(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE name='ledger_tx_fts'",
        [],
        |_| Ok(()),
    )
    .optional()
    .ok()
    .flatten()
    .is_some()
}

/// Remove deleted pages as well as rows: erased history must not linger in the file or its WAL.
fn scrub(conn: &Connection) {
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
}

fn purge_all(conn: &Connection) -> Result<usize, String> {
    if !schema_present(conn) {
        return Ok(0);
    }
    let held: i64 = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM ledger_tx_conversations)+(SELECT COUNT(*) FROM ledger_tx_sources)
                    +(SELECT COUNT(*) FROM ledger_tx_files)+(SELECT COUNT(*) FROM ledger_tx_messages)",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if held == 0 {
        return Ok(0);
    }
    let _ = conn.execute_batch("PRAGMA secure_delete=ON;");
    let tx = Tx::begin(conn)?;
    let mut rows = 0usize;
    for table in TX_TABLES {
        rows += conn
            .execute(&format!("DELETE FROM {table}"), [])
            .map_err(|e| e.to_string())?;
    }
    conn.execute("DELETE FROM ledger_tx_fts", [])
        .map_err(|e| e.to_string())?;
    tx.commit()?;
    scrub(conn);
    Ok(rows)
}

/// Manual transaction guard so helpers can take a plain `&Connection` and a batch can span
/// callbacks. Rolls back unless committed.
struct Tx<'a> {
    conn: &'a Connection,
    done: bool,
}
impl<'a> Tx<'a> {
    fn begin(conn: &'a Connection) -> Result<Self, String> {
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| e.to_string())?;
        Ok(Self { conn, done: false })
    }
    fn commit(mut self) -> Result<(), String> {
        self.done = true;
        self.conn.execute_batch("COMMIT").map_err(|e| e.to_string())
    }
}
impl Drop for Tx<'_> {
    fn drop(&mut self) {
        if !self.done {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }
}

// ---- durable denials ----

struct Denials {
    conversations: BTreeSet<String>,
    sources: BTreeSet<String>,
}

fn load_denials(catalog: &ContextCatalog) -> Result<Denials, String> {
    let conn = catalog.lock();
    conn.execute_batch(CATALOG_SCHEMA)
        .map_err(|_| "ledger_policy_store_unavailable")?;
    let read = |sql: &str| -> Result<BTreeSet<String>, String> {
        let mut statement = conn
            .prepare(sql)
            .map_err(|_| "ledger_policy_store_unavailable".to_string())?;
        let rows = statement
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|_| "ledger_policy_store_unavailable".to_string())?
            .collect::<Result<BTreeSet<_>, _>>()
            .map_err(|_| "ledger_policy_store_unavailable".to_string())?;
        Ok(rows)
    };
    let conversations = read(&format!(
        "SELECT conv_key FROM ledger_tx_tombstones LIMIT {}",
        MAX_TOMBSTONES + 1
    ))?;
    if conversations.len() > MAX_TOMBSTONES {
        return Err("ledger_policy_budget_exhausted".into());
    }
    let sources = read("SELECT source_id FROM ledger_tx_source_exclusions")?;
    Ok(Denials {
        conversations,
        sources,
    })
}

// ---- file listing ----

#[derive(Clone, Copy, PartialEq, Eq)]
enum FileKind {
    Ingest,
    Unsupported,
}

struct Listed {
    abs: PathBuf,
    rel: String,
    size: u64,
    mtime_ms: i64,
    kind: FileKind,
}

fn classify(provider: &str, name: &str) -> Option<FileKind> {
    match provider {
        "codex" if name.starts_with("rollout-") && name.ends_with(".jsonl") => {
            Some(FileKind::Ingest)
        }
        // Archived rollouts are zstd; there is no decoder in this build. Recorded, not guessed.
        "codex" if name.starts_with("rollout-") && name.ends_with(".jsonl.zst") => {
            Some(FileKind::Unsupported)
        }
        "chatgpt" if name.starts_with("conversations") && name.ends_with(".json") => {
            Some(FileKind::Ingest)
        }
        _ => None,
    }
}

/// Files of one source, confined to its root: symlinks are skipped, never followed. The flag says
/// whether the walk saw every directory; only a complete walk may mark files missing.
fn list_files(
    source: &Source,
    root: &Path,
    budget: &WorkBudget,
) -> Result<(Vec<Listed>, bool), String> {
    let mut out = Vec::new();
    let mut complete = true;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        budget.check()?;
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) if dir == root => return Err("ledger_transcript_source_unavailable".into()),
            Err(_) => {
                complete = false;
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                complete = false;
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                if depth < MAX_WALK_DEPTH {
                    stack.push((path, depth + 1));
                } else {
                    complete = false;
                }
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(kind) = classify(&source.provider, name) else {
                continue;
            };
            if out.len() >= MAX_FILES_PER_SOURCE {
                complete = false;
                break;
            }
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let mtime_ms = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            out.push(Listed {
                rel: relative.to_string_lossy().replace('\\', "/"),
                abs: path,
                size: metadata.len(),
                mtime_ms,
                kind,
            });
        }
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok((out, complete))
}

// ---- reports ----

#[derive(Default, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    pub enabled: bool,
    pub purged_rows: usize,
    pub sources: usize,
    pub sources_unavailable: usize,
    pub sources_excluded: usize,
    pub files_scanned: usize,
    pub files_unchanged: usize,
    pub files_ingested: usize,
    pub files_failed: usize,
    pub files_unsupported: usize,
    pub files_missing: usize,
    pub conversations_touched: usize,
    pub conversations_tombstoned: usize,
    pub messages_new: usize,
    pub messages_revised: usize,
    pub messages_replayed: usize,
    pub messages_seen_on_another_device: usize,
    pub skipped_reasoning: usize,
    pub skipped_instructions: usize,
    pub skipped_hidden: usize,
    pub skipped_unsupported: usize,
    pub bad_lines: usize,
    /// False when a pass stopped early (deadline, cancel); the cursors resume it next cycle.
    pub complete: bool,
}

#[derive(Default)]
struct Tally {
    conversations: BTreeSet<String>,
    tombstoned: usize,
    new: usize,
    revised: usize,
    replayed: usize,
    other_device: usize,
    skipped: parse::Skipped,
}

enum Outcome {
    New,
    Revised,
    Replayed,
    OtherDevice,
}

fn is_stop(error: &str) -> bool {
    error.starts_with("ledger_cancelled") || error.starts_with("ledger_deadline_exhausted")
}

// ---- store writes ----

struct ConvWrite<'a> {
    key: &'a str,
    scope: &'a str,
    provider: &'a str,
    meta: &'a parse::ConversationMeta,
    title: &'a str,
    title_overwrite: bool,
}

/// Returns the conversation's `updated_ms` before this write (0 when new).
fn upsert_conversation(conn: &Connection, c: &ConvWrite<'_>) -> Result<i64, String> {
    let prior: i64 = conn
        .query_row(
            "SELECT updated_ms FROM ledger_tx_conversations WHERE conv_key=?1",
            [c.key],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .unwrap_or(0);
    conn.execute(
        "INSERT INTO ledger_tx_conversations(conv_key,scope,provider,provider_conv_id,title,created_ms,
            updated_ms,branch,workspace,parent_conv_id,current_msg_id,state)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'active')
         ON CONFLICT(conv_key) DO UPDATE SET
           title=CASE WHEN excluded.title<>'' AND (title='' OR (?12 AND excluded.updated_ms>=updated_ms))
                           THEN excluded.title ELSE title END,
           created_ms=CASE WHEN excluded.created_ms>0 AND (created_ms=0 OR excluded.created_ms<created_ms)
                           THEN excluded.created_ms ELSE created_ms END,
           current_msg_id=CASE WHEN excluded.current_msg_id IS NOT NULL AND excluded.updated_ms>=updated_ms
                           THEN excluded.current_msg_id ELSE current_msg_id END,
           updated_ms=MAX(updated_ms,excluded.updated_ms),
           branch=COALESCE(excluded.branch,branch),
           workspace=COALESCE(excluded.workspace,workspace),
           parent_conv_id=COALESCE(excluded.parent_conv_id,parent_conv_id)
         WHERE state='active'",
        params![
            c.key,
            c.scope,
            c.provider,
            c.meta.id,
            c.title,
            c.meta.created_ms,
            c.meta.updated_ms,
            c.meta.branch,
            c.meta.workspace,
            c.meta.parent_conv_id,
            c.meta.current_msg_id,
            c.title_overwrite
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(prior)
}

fn searchable(m: &parse::ParsedMessage) -> bool {
    m.kind == "text" && matches!(m.role, "user" | "assistant")
}

fn drop_chunks(conn: &Connection, key: &str, keep_revision: &str) -> Result<(), String> {
    conn.execute(
        "DELETE FROM ledger_tx_fts WHERE rowid IN
           (SELECT chunk_id FROM ledger_tx_chunks WHERE msg_key=?1 AND revision<>?2)",
        params![key, keep_revision],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "DELETE FROM ledger_tx_chunks WHERE msg_key=?1 AND revision<>?2",
        params![key, keep_revision],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn add_chunks(
    conn: &Connection,
    key: &str,
    revision: &str,
    m: &parse::ParsedMessage,
) -> Result<(), String> {
    if !searchable(m) {
        return Ok(());
    }
    for (ix, _offset, body) in parse::chunks(&m.text) {
        conn.execute(
            "INSERT INTO ledger_tx_chunks(msg_key,revision,ix) VALUES(?1,?2,?3)",
            params![key, revision, ix as i64],
        )
        .map_err(|e| e.to_string())?;
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO ledger_tx_fts(rowid, body) VALUES(?1,?2)",
            params![id, body],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

struct Where<'a> {
    source: &'a Source,
    rel: &'a str,
    now: i64,
    /// This export is at least as new as the stored conversation: its path flags win.
    newer: bool,
}

fn upsert_message(
    conn: &Connection,
    ck: &str,
    m: &parse::ParsedMessage,
    at: &Where<'_>,
) -> Result<Outcome, String> {
    let key = msg_key(ck, &m.provider_msg_id);
    let revision = m.revision();
    let existing: Option<String> = conn
        .query_row(
            "SELECT state FROM ledger_tx_messages WHERE msg_key=?1 AND revision=?2",
            params![key, revision],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let had_other: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM ledger_tx_messages WHERE msg_key=?1 AND revision<>?2)",
            params![key, revision],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let this_seen: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM ledger_tx_seen WHERE msg_key=?1 AND revision=?2
               AND device=?3 AND source_id=?4)",
            params![key, revision, at.source.device, at.source.id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let outcome = match existing.as_deref() {
        Some("current") => {
            if at.newer {
                conn.execute(
                    "UPDATE ledger_tx_messages SET on_current_path=?3 WHERE msg_key=?1 AND revision=?2",
                    params![key, revision, m.on_current_path],
                )
                .map_err(|e| e.to_string())?;
            }
            if this_seen {
                Outcome::Replayed
            } else {
                Outcome::OtherDevice
            }
        }
        Some(_) => {
            // The older text came back: it is the current revision again.
            conn.execute(
                "UPDATE ledger_tx_messages SET state=CASE WHEN revision=?2 THEN 'current' ELSE 'superseded' END
                 WHERE msg_key=?1",
                params![key, revision],
            )
            .map_err(|e| e.to_string())?;
            drop_chunks(conn, &key, &revision)?;
            add_chunks(conn, &key, &revision, m)?;
            Outcome::Revised
        }
        None => {
            conn.execute(
                "UPDATE ledger_tx_messages SET state='superseded' WHERE msg_key=?1",
                [&key],
            )
            .map_err(|e| e.to_string())?;
            drop_chunks(conn, &key, &revision)?;
            conn.execute(
                "INSERT INTO ledger_tx_messages(msg_key,revision,conv_key,provider_msg_id,parent_msg_id,role,
                    kind,ordinal,created_ms,text,text_sha256,truncated,on_current_path,state,first_seen_ms)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'current',?14)",
                params![
                    key,
                    revision,
                    ck,
                    m.provider_msg_id,
                    m.parent_msg_id,
                    m.role,
                    m.kind,
                    m.ordinal,
                    m.created_ms,
                    m.text,
                    sha256_bytes(m.text.as_bytes()),
                    m.truncated,
                    m.on_current_path,
                    at.now
                ],
            )
            .map_err(|e| e.to_string())?;
            add_chunks(conn, &key, &revision, m)?;
            if had_other {
                Outcome::Revised
            } else {
                Outcome::New
            }
        }
    };
    conn.execute(
        "INSERT INTO ledger_tx_seen(msg_key,revision,device,source_id,rel_path,byte_start,byte_end,raw_sha256,locator,seen_ms)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
         ON CONFLICT(msg_key,revision,device,source_id) DO UPDATE SET rel_path=excluded.rel_path,
           byte_start=excluded.byte_start,byte_end=excluded.byte_end,raw_sha256=excluded.raw_sha256,
           locator=excluded.locator,seen_ms=excluded.seen_ms",
        params![
            key,
            revision,
            at.source.device,
            at.source.id,
            at.rel,
            m.byte_start,
            m.byte_end,
            m.raw_sha256,
            m.locator,
            at.now
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(outcome)
}

// ---- file cursors ----

#[derive(Clone)]
struct FileRow {
    size: i64,
    mtime_ms: i64,
    consumed: i64,
    lines: i64,
    prefix: String,
    parser: String,
    conv_id: Option<String>,
    state: String,
}

fn load_files(
    conn: &Connection,
    source_id: &str,
) -> Result<std::collections::BTreeMap<String, FileRow>, String> {
    let mut statement = conn
        .prepare(
            "SELECT rel_path,size,mtime_ms,consumed_bytes,lines,prefix_sha256,parser_version,conv_id,state
             FROM ledger_tx_files WHERE source_id=?1",
        )
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([source_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                FileRow {
                    size: r.get(1)?,
                    mtime_ms: r.get(2)?,
                    consumed: r.get(3)?,
                    lines: r.get(4)?,
                    prefix: r.get(5)?,
                    parser: r.get(6)?,
                    conv_id: r.get(7)?,
                    state: r.get(8)?,
                },
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<std::collections::BTreeMap<_, _>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

fn save_file(
    conn: &Connection,
    source_id: &str,
    rel: &str,
    row: &FileRow,
    detail: &str,
    now: i64,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO ledger_tx_files(source_id,rel_path,size,mtime_ms,consumed_bytes,lines,prefix_sha256,
            parser_version,conv_id,state,detail,updated_ms)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
         ON CONFLICT(source_id,rel_path) DO UPDATE SET size=excluded.size,mtime_ms=excluded.mtime_ms,
           consumed_bytes=excluded.consumed_bytes,lines=excluded.lines,prefix_sha256=excluded.prefix_sha256,
           parser_version=excluded.parser_version,conv_id=excluded.conv_id,state=excluded.state,
           detail=excluded.detail,updated_ms=excluded.updated_ms",
        params![
            source_id,
            rel,
            row.size,
            row.mtime_ms,
            row.consumed,
            row.lines,
            row.prefix,
            row.parser,
            row.conv_id,
            row.state,
            detail,
            now
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn hash_prefix(file: &mut File, hasher: &mut Sha256Hasher, mut remaining: u64) -> bool {
    let mut buffer = vec![0u8; 1 << 20];
    while remaining > 0 {
        let want = remaining.min(buffer.len() as u64) as usize;
        match file.read(&mut buffer[..want]) {
            Ok(0) | Err(_) => return false,
            Ok(n) => {
                hasher.update(&buffer[..n]);
                remaining -= n as u64;
            }
        }
    }
    true
}

fn first_words(text: &str) -> String {
    text.chars()
        .take(80)
        .collect::<String>()
        .replace(['\n', '\r'], " ")
        .trim()
        .to_string()
}

// ---- Codex ----

#[allow(clippy::too_many_arguments)]
fn ingest_codex(
    conn: &Connection,
    source: &Source,
    file: &Listed,
    prior: Option<&FileRow>,
    denials: &Denials,
    now: i64,
    budget: &WorkBudget,
    tally: &mut Tally,
) -> Result<bool, String> {
    if let Some(p) = prior {
        if p.parser == parse::PARSER_VERSION
            && p.state == "present"
            && p.size == file.size as i64
            && p.mtime_ms == file.mtime_ms
        {
            return Ok(false);
        }
    }
    let mut handle = File::open(&file.abs).map_err(|_| "ledger_transcript_file_unreadable")?;
    let mut hasher = Sha256Hasher::new();
    let (mut start, mut lines, mut conv_id) = (0u64, 0i64, None::<String>);
    if let Some(p) = prior {
        if p.parser == parse::PARSER_VERSION && p.consumed > 0 && p.consumed as u64 <= file.size {
            let mut candidate = Sha256Hasher::new();
            if hash_prefix(&mut handle, &mut candidate, p.consumed as u64)
                && candidate.clone().finalize() == p.prefix
            {
                hasher = candidate;
                start = p.consumed as u64;
                lines = p.lines;
                conv_id = p.conv_id.clone();
            } else {
                // The head of the file changed: it was rewritten, not appended. Reparse it all;
                // changed messages become new revisions, unchanged ones replay for free.
                handle
                    .seek(SeekFrom::Start(0))
                    .map_err(|_| "ledger_transcript_file_unreadable")?;
            }
        }
    }
    let want = file.size.saturating_sub(start).min(MAX_TAIL_BYTES);
    let capped = file.size.saturating_sub(start) > MAX_TAIL_BYTES;
    let mut tail = Vec::with_capacity(want as usize);
    handle
        .by_ref()
        .take(want)
        .read_to_end(&mut tail)
        .map_err(|_| "ledger_transcript_file_unreadable")?;
    let used = tail.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    if used == 0 && capped {
        // One line longer than a whole pass: refuse the file rather than loop on it.
        return Err("ledger_transcript_line_too_long".into());
    }
    hasher.update(&tail[..used]);
    let consumed = start + used as u64;

    let mut meta: Option<parse::ConversationMeta> = None;
    let mut messages = Vec::new();
    let mut offset = 0usize;
    let mut line_no = lines;
    while offset < used {
        budget.check()?;
        let end = tail[offset..used]
            .iter()
            .position(|b| *b == b'\n')
            .map_or(used, |i| offset + i);
        let mut line = &tail[offset..end];
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        line_no += 1;
        if !line.is_empty() {
            match parse::codex_line(
                line,
                start as i64 + offset as i64,
                line_no,
                &mut tally.skipped,
            ) {
                parse::CodexLine::Meta(m) => {
                    if !m.id.is_empty() {
                        conv_id = Some(m.id.clone());
                    }
                    meta = Some(m);
                }
                parse::CodexLine::Message(m) => messages.push(m),
                parse::CodexLine::Skip => {}
            }
        }
        offset = end + 1;
    }
    let id = conv_id
        .clone()
        .or_else(|| {
            Path::new(&file.rel)
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(parse::codex_id_from_file_name)
        })
        .unwrap_or_else(|| format!("file:{}", sha256_bytes(file.rel.as_bytes())));
    let ck = conv_key(&source.scope, &source.provider, &id);
    let tx = Tx::begin(conn)?;
    if denials.conversations.contains(&ck) {
        tally.tombstoned += 1;
    } else if !messages.is_empty() || meta.is_some() {
        let mut m = meta.unwrap_or_default();
        m.id = id.clone();
        m.updated_ms = messages.iter().map(|x| x.created_ms).max().unwrap_or(0);
        if m.created_ms == 0 {
            m.created_ms = messages.iter().map(|x| x.created_ms).min().unwrap_or(0);
        }
        let title = messages
            .iter()
            .find(|x| x.role == "user" && x.kind == "text")
            .map(|x| first_words(&x.text))
            .unwrap_or_default();
        upsert_conversation(
            conn,
            &ConvWrite {
                key: &ck,
                scope: &source.scope,
                provider: &source.provider,
                meta: &m,
                title: &title,
                title_overwrite: false,
            },
        )?;
        tally.conversations.insert(ck.clone());
        let at = Where {
            source,
            rel: &file.rel,
            now,
            newer: true,
        };
        for message in &messages {
            budget.visit()?;
            record(tally, upsert_message(conn, &ck, message, &at)?);
        }
    }
    save_file(
        conn,
        &source.id,
        &file.rel,
        &FileRow {
            // A capped pass stores the consumed size so the next cycle sees a difference and
            // continues; a complete one stores the stat size so an idle file is skipped.
            size: if capped {
                consumed as i64
            } else {
                file.size as i64
            },
            mtime_ms: file.mtime_ms,
            consumed: consumed as i64,
            lines: line_no,
            prefix: hasher.finalize(),
            parser: parse::PARSER_VERSION.into(),
            conv_id: Some(id),
            state: "present".into(),
        },
        "",
        now,
    )?;
    tx.commit()?;
    Ok(true)
}

fn record(tally: &mut Tally, outcome: Outcome) {
    match outcome {
        Outcome::New => tally.new += 1,
        Outcome::Revised => tally.revised += 1,
        Outcome::Replayed => tally.replayed += 1,
        Outcome::OtherDevice => tally.other_device += 1,
    }
}

// ---- ChatGPT export ----

/// Visit each element of a top-level JSON array without holding the whole document.
fn each_array_element(
    path: &Path,
    visit: &mut dyn FnMut(serde_json::Value) -> Result<(), String>,
) -> Result<(), String> {
    use serde::de::{Deserializer, SeqAccess, Visitor};
    struct Walk<'a> {
        visit: &'a mut dyn FnMut(serde_json::Value) -> Result<(), String>,
        error: &'a mut Option<String>,
    }
    impl<'de, 'a> Visitor<'de> for Walk<'a> {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an array of conversations")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
            while let Some(value) = seq.next_element::<serde_json::Value>()? {
                if let Err(e) = (self.visit)(value) {
                    *self.error = Some(e);
                    return Err(serde::de::Error::custom("stopped"));
                }
            }
            Ok(())
        }
    }
    let file = File::open(path).map_err(|_| "ledger_transcript_file_unreadable")?;
    let mut error = None;
    let mut de = serde_json::Deserializer::from_reader(BufReader::new(file));
    let result = de.deserialize_seq(Walk {
        visit,
        error: &mut error,
    });
    if let Some(e) = error {
        return Err(e);
    }
    result.map_err(|_| "ledger_transcript_export_unsupported_shape".to_string())
}

#[allow(clippy::too_many_arguments)]
fn ingest_chatgpt(
    conn: &Connection,
    source: &Source,
    file: &Listed,
    prior: Option<&FileRow>,
    denials: &Denials,
    now: i64,
    budget: &WorkBudget,
    tally: &mut Tally,
) -> Result<bool, String> {
    if file.size > MAX_EXPORT_BYTES {
        return Err("ledger_transcript_export_too_large".into());
    }
    if let Some(p) = prior {
        if p.parser == parse::PARSER_VERSION
            && p.state == "present"
            && p.size == file.size as i64
            && p.mtime_ms == file.mtime_ms
        {
            return Ok(false);
        }
    }
    let digest = sha256_file(&file.abs).map_err(|_| "ledger_transcript_file_unreadable")?;
    let unchanged = |row: &FileRow| row.parser == parse::PARSER_VERSION && row.prefix == digest;
    if let Some(p) = prior.filter(|p| p.state == "present" && unchanged(p)) {
        // Same bytes under a new mtime: remember the stat, change nothing else.
        let mut row = p.clone();
        row.size = file.size as i64;
        row.mtime_ms = file.mtime_ms;
        save_file(conn, &source.id, &file.rel, &row, "", now)?;
        return Ok(false);
    }
    let mut batch: Option<Tx<'_>> = None;
    let mut in_batch = 0usize;
    let mut walk = |value: serde_json::Value| -> Result<(), String> {
        budget.check()?;
        let Some((meta, messages)) = parse::chatgpt_conversation(&value, &mut tally.skipped) else {
            tally.skipped.unsupported += 1;
            return Ok(());
        };
        let ck = conv_key(&source.scope, &source.provider, &meta.id);
        if denials.conversations.contains(&ck) {
            tally.tombstoned += 1;
            return Ok(());
        }
        if batch.is_none() {
            batch = Some(Tx::begin(conn)?);
        }
        let prior_updated = upsert_conversation(
            conn,
            &ConvWrite {
                key: &ck,
                scope: &source.scope,
                provider: &source.provider,
                meta: &meta,
                title: &meta.title,
                title_overwrite: true,
            },
        )?;
        tally.conversations.insert(ck.clone());
        let at = Where {
            source,
            rel: &file.rel,
            now,
            newer: meta.updated_ms >= prior_updated,
        };
        for message in &messages {
            budget.visit()?;
            record(tally, upsert_message(conn, &ck, message, &at)?);
        }
        in_batch += 1;
        if in_batch >= BATCH_CONVERSATIONS {
            if let Some(done) = batch.take() {
                done.commit()?;
            }
            in_batch = 0;
        }
        Ok(())
    };
    each_array_element(&file.abs, &mut walk)?;
    if let Some(done) = batch.take() {
        done.commit()?;
    }
    save_file(
        conn,
        &source.id,
        &file.rel,
        &FileRow {
            size: file.size as i64,
            mtime_ms: file.mtime_ms,
            consumed: file.size as i64,
            lines: 0,
            prefix: digest,
            parser: parse::PARSER_VERSION.into(),
            conv_id: None,
            state: "present".into(),
        },
        "",
        now,
    )?;
    Ok(true)
}

// ---- maintenance ----

/// One maintenance pass over the manifest's sources. Runs only on the Ledger owner's writer
/// connection, under background authority, bounded by `budget`. Per-file failures are recorded
/// (with a typed code, never content) and do not stop the pass; a deadline or cancellation does,
/// after the files already committed.
pub fn maintain(
    db: &LedgerDb,
    catalog: &ContextCatalog,
    manifest: &Path,
    budget: &WorkBudget,
) -> Result<SyncReport, String> {
    let manifest = load_manifest(manifest)?;
    let conn = db.lock();
    let mut report = SyncReport {
        complete: true,
        ..Default::default()
    };
    let Some(manifest) = manifest.filter(|m| m.enabled) else {
        report.purged_rows = purge_all(&conn)?;
        return Ok(report);
    };
    report.enabled = true;
    ensure_schema(&conn)?;
    let denials = load_denials(catalog)?;
    let listed_ids: BTreeSet<&str> = manifest.sources.iter().map(|s| s.id.as_str()).collect();
    // A source dropped from the manifest is no longer authorized: its history leaves the index.
    let stale: Vec<String> = {
        let mut statement = conn
            .prepare("SELECT source_id FROM ledger_tx_sources")
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        rows.into_iter()
            .filter(|id| !listed_ids.contains(id.as_str()))
            .collect()
    };
    for id in stale {
        remove_source_rows(&conn, &id)?;
    }
    report.sources = manifest.sources.len();
    let mut tally = Tally::default();
    for source in &manifest.sources {
        if let Err(error) = budget.check() {
            report.complete = false;
            finish(&mut report, &tally);
            return if is_stop(&error) {
                Ok(report)
            } else {
                Err(error)
            };
        }
        if denials.sources.contains(&source.id) {
            report.sources_excluded += 1;
            remove_source_rows(&conn, &source.id)?;
            continue;
        }
        match sync_source(&conn, source, &denials, budget, &mut tally, &mut report) {
            Ok(()) => {}
            Err(error) if is_stop(&error) => {
                report.complete = false;
                break;
            }
            Err(error) => {
                report.sources_unavailable += 1;
                record_source(&conn, source, "source_unavailable", &error, 0, 0)?;
            }
        }
    }
    finish(&mut report, &tally);
    Ok(report)
}

fn finish(report: &mut SyncReport, tally: &Tally) {
    report.conversations_touched = tally.conversations.len();
    report.conversations_tombstoned = tally.tombstoned;
    report.messages_new = tally.new;
    report.messages_revised = tally.revised;
    report.messages_replayed = tally.replayed;
    report.messages_seen_on_another_device = tally.other_device;
    report.skipped_reasoning = tally.skipped.reasoning;
    report.skipped_instructions = tally.skipped.instructions;
    report.skipped_hidden = tally.skipped.hidden;
    report.skipped_unsupported = tally.skipped.unsupported;
    report.bad_lines = tally.skipped.bad_lines;
}

fn record_source(
    conn: &Connection,
    source: &Source,
    status: &str,
    detail: &str,
    seen: usize,
    failed: usize,
) -> Result<(), String> {
    let detail = detail
        .split(':')
        .next()
        .filter(|d| d.len() <= 96 && d.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        .unwrap_or("");
    conn.execute(
        "INSERT INTO ledger_tx_sources(source_id,device,provider,kind,scope,root_digest,last_scan_ms,status,
            detail,files_seen,files_failed)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
         ON CONFLICT(source_id) DO UPDATE SET device=excluded.device,provider=excluded.provider,
           kind=excluded.kind,scope=excluded.scope,root_digest=excluded.root_digest,
           last_scan_ms=excluded.last_scan_ms,status=excluded.status,detail=excluded.detail,
           files_seen=excluded.files_seen,files_failed=excluded.files_failed",
        params![
            source.id,
            source.device,
            source.provider,
            if source.provider == "codex" { "codex_rollouts" } else { "chatgpt_export" },
            source.scope,
            sha256_bytes(source.root.as_bytes()),
            now_ms(),
            status,
            detail,
            seen as i64,
            failed as i64
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn sync_source(
    conn: &Connection,
    source: &Source,
    denials: &Denials,
    budget: &WorkBudget,
    tally: &mut Tally,
    report: &mut SyncReport,
) -> Result<(), String> {
    let root = std::fs::canonicalize(&source.root)
        .ok()
        .filter(|p| p.is_dir())
        .ok_or("ledger_transcript_source_unavailable")?;
    let (files, complete) = list_files(source, &root, budget)?;
    let prior = load_files(conn, &source.id)?;
    let now = now_ms();
    let mut failed = 0usize;
    for file in &files {
        budget.check()?;
        report.files_scanned += 1;
        if file.kind == FileKind::Unsupported {
            report.files_unsupported += 1;
            save_file(
                conn,
                &source.id,
                &file.rel,
                &FileRow {
                    size: file.size as i64,
                    mtime_ms: file.mtime_ms,
                    consumed: 0,
                    lines: 0,
                    prefix: String::new(),
                    parser: parse::PARSER_VERSION.into(),
                    conv_id: None,
                    state: "unsupported".into(),
                },
                "compressed_unsupported",
                now,
            )?;
            continue;
        }
        let before = prior.get(&file.rel);
        let result = if source.provider == "codex" {
            ingest_codex(conn, source, file, before, denials, now, budget, tally)
        } else {
            ingest_chatgpt(conn, source, file, before, denials, now, budget, tally)
        };
        match result {
            Ok(true) => report.files_ingested += 1,
            Ok(false) => report.files_unchanged += 1,
            Err(error) if is_stop(&error) => return Err(error),
            Err(error) => {
                failed += 1;
                report.files_failed += 1;
                let code = error
                    .split(':')
                    .next()
                    .filter(|d| {
                        d.len() <= 96 && d.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    })
                    .unwrap_or("ledger_transcript_file_failed");
                // Keep the cursor of what was committed; mark only the failure.
                let mut row = before.cloned().unwrap_or(FileRow {
                    size: 0,
                    mtime_ms: 0,
                    consumed: 0,
                    lines: 0,
                    prefix: String::new(),
                    parser: String::new(),
                    conv_id: None,
                    state: "failed".into(),
                });
                row.state = "failed".into();
                row.size = -1;
                save_file(conn, &source.id, &file.rel, &row, code, now)?;
            }
        }
    }
    let mut missing = 0usize;
    if complete {
        // A file that left the folder keeps its history; it is only labelled.
        let present: BTreeSet<&str> = files.iter().map(|f| f.rel.as_str()).collect();
        for (rel, row) in &prior {
            if row.state == "present" && !present.contains(rel.as_str()) {
                conn.execute(
                    "UPDATE ledger_tx_files SET state='missing',updated_ms=?3 WHERE source_id=?1 AND rel_path=?2",
                    params![source.id, rel, now],
                )
                .map_err(|e| e.to_string())?;
                missing += 1;
            }
        }
    }
    report.files_missing += missing;
    record_source(
        conn,
        source,
        if failed > 0 {
            "partial"
        } else if complete {
            "ok"
        } else {
            "incomplete_walk"
        },
        "",
        files.len(),
        failed,
    )
}

// ---- erasure ----

fn remove_source_rows(conn: &Connection, source_id: &str) -> Result<(), String> {
    let _ = conn.execute_batch("PRAGMA secure_delete=ON;");
    let tx = Tx::begin(conn)?;
    for sql in [
        "DELETE FROM ledger_tx_seen WHERE source_id=?1",
        "DELETE FROM ledger_tx_fts WHERE rowid IN (SELECT c.chunk_id FROM ledger_tx_chunks c
           WHERE NOT EXISTS(SELECT 1 FROM ledger_tx_seen s WHERE s.msg_key=c.msg_key AND s.revision=c.revision))",
        "DELETE FROM ledger_tx_chunks WHERE NOT EXISTS(SELECT 1 FROM ledger_tx_seen s
           WHERE s.msg_key=ledger_tx_chunks.msg_key AND s.revision=ledger_tx_chunks.revision)",
        "DELETE FROM ledger_tx_messages WHERE NOT EXISTS(SELECT 1 FROM ledger_tx_seen s
           WHERE s.msg_key=ledger_tx_messages.msg_key AND s.revision=ledger_tx_messages.revision)",
        "DELETE FROM ledger_tx_conversations WHERE NOT EXISTS(SELECT 1 FROM ledger_tx_messages m
           WHERE m.conv_key=ledger_tx_conversations.conv_key)",
        "DELETE FROM ledger_tx_files WHERE source_id=?1",
        "DELETE FROM ledger_tx_sources WHERE source_id=?1",
    ] {
        // `?1` is absent from the orphan statements; bind only where it is named.
        let result = if sql.contains("?1") {
            conn.execute(sql, [source_id])
        } else {
            conn.execute(sql, [])
        };
        result.map_err(|e| e.to_string())?;
    }
    tx.commit()?;
    scrub(conn);
    Ok(())
}

/// Erase one conversation (by the `convKey` a hit carries) and keep it erased: the tombstone is a
/// durable catalog record, so neither a rescan nor an index rebuild brings it back.
pub fn erase_conversation(
    db: &LedgerDb,
    catalog: &ContextCatalog,
    conv_key: &str,
) -> Result<usize, String> {
    if !is_hex64(conv_key) {
        return Err("ledger_transcript_key_invalid".into());
    }
    let conn = db.lock();
    let scope: String = if schema_present(&conn) {
        conn.query_row(
            "SELECT scope FROM ledger_tx_conversations WHERE conv_key=?1",
            [conv_key],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .unwrap_or_default()
    } else {
        String::new()
    };
    {
        // The deny record first: if the index delete then fails, the next pass still refuses it.
        let catalog_conn = catalog.lock();
        catalog_conn
            .execute_batch(CATALOG_SCHEMA)
            .map_err(|_| "ledger_policy_store_unavailable")?;
        catalog_conn
            .execute(
                "INSERT OR IGNORE INTO ledger_tx_tombstones VALUES (?1,?2,?3)",
                params![conv_key, scope, now_ms()],
            )
            .map_err(|_| "ledger_policy_store_unavailable")?;
    }
    if !schema_present(&conn) {
        return Ok(0);
    }
    let _ = conn.execute_batch("PRAGMA secure_delete=ON;");
    let tx = Tx::begin(&conn)?;
    let mut removed = 0usize;
    for sql in [
        "DELETE FROM ledger_tx_fts WHERE rowid IN (SELECT c.chunk_id FROM ledger_tx_chunks c
           JOIN ledger_tx_messages m ON m.msg_key=c.msg_key AND m.revision=c.revision WHERE m.conv_key=?1)",
        "DELETE FROM ledger_tx_chunks WHERE msg_key IN (SELECT msg_key FROM ledger_tx_messages WHERE conv_key=?1)",
        "DELETE FROM ledger_tx_seen WHERE msg_key IN (SELECT msg_key FROM ledger_tx_messages WHERE conv_key=?1)",
        "DELETE FROM ledger_tx_messages WHERE conv_key=?1",
        "DELETE FROM ledger_tx_conversations WHERE conv_key=?1",
    ] {
        removed += conn.execute(sql, [conv_key]).map_err(|e| e.to_string())?;
    }
    tx.commit()?;
    scrub(&conn);
    Ok(removed)
}

/// Erase everything a source contributed and exclude it until the exclusion is lifted; a message
/// also seen through another source stays, cited from that source only.
pub fn erase_source(
    db: &LedgerDb,
    catalog: &ContextCatalog,
    source_id: &str,
) -> Result<(), String> {
    if !label_ok(source_id) {
        return Err("ledger_transcript_key_invalid".into());
    }
    {
        let catalog_conn = catalog.lock();
        catalog_conn
            .execute_batch(CATALOG_SCHEMA)
            .map_err(|_| "ledger_policy_store_unavailable")?;
        catalog_conn
            .execute(
                "INSERT OR IGNORE INTO ledger_tx_source_exclusions VALUES (?1,?2)",
                params![source_id, now_ms()],
            )
            .map_err(|_| "ledger_policy_store_unavailable")?;
    }
    let conn = db.lock();
    if schema_present(&conn) {
        remove_source_rows(&conn, source_id)?;
    }
    Ok(())
}

// ---- search ----

#[derive(Clone, Debug, Default)]
pub struct TxQuery {
    /// Scopes the caller may read. Required: an empty list is refused, never "all".
    pub scopes: Vec<String>,
    pub text: String,
    pub limit: usize,
    pub provider: Option<String>,
    pub role: Option<String>,
    pub since_ms: Option<i64>,
    pub until_ms: Option<i64>,
    /// Only messages on the branch the conversation ended on (ChatGPT edits and regenerations
    /// sit off it).
    pub current_path_only: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Citation {
    pub device: String,
    pub source_id: String,
    pub rel_path: String,
    pub byte_start: i64,
    pub byte_end: i64,
    pub locator: String,
    pub raw_sha256: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TxHit {
    pub conv_key: String,
    pub msg_key: String,
    pub revision: String,
    pub scope: String,
    pub provider: String,
    pub provider_conv_id: String,
    pub provider_msg_id: String,
    pub title: String,
    pub branch: Option<String>,
    pub workspace: Option<String>,
    pub role: String,
    pub created_ms: i64,
    pub on_current_path: bool,
    pub snippet: String,
    pub score: f64,
    /// Every device and source the same message revision was read from.
    pub citations: Vec<Citation>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TxResult {
    pub hits: Vec<TxHit>,
    /// `and` when every term matched, `or` when the search fell back to any term.
    pub match_mode: &'static str,
    pub omissions: Vec<String>,
}

/// The current manifest must be on and name every scope asked for; checked on each read, against
/// the file as it is now, never against what the index happens to hold. Off, absent, unreadable or
/// not naming a scope all refuse (fail closed).
pub fn authorize_scopes(manifest: &Path, scopes: &[String]) -> Result<(), String> {
    if scopes.is_empty() || scopes.iter().any(|s| !label_ok(s)) {
        return Err("ledger_transcript_scope_required".into());
    }
    match load_manifest(manifest)? {
        Some(m) if m.enabled => {
            if scopes
                .iter()
                .all(|scope| m.sources.iter().any(|source| &source.scope == scope))
            {
                Ok(())
            } else {
                Err("ledger_transcript_scope_not_enrolled".into())
            }
        }
        _ => Err("ledger_transcripts_off".into()),
    }
}

pub fn search(
    db: &LedgerDb,
    manifest: &Path,
    q: &TxQuery,
    budget: &WorkBudget,
) -> Result<TxResult, String> {
    authorize_scopes(manifest, &q.scopes)?;
    let terms = index::query_terms(&q.text);
    if terms.is_empty() {
        return Err("ledger_transcript_query_empty".into());
    }
    let limit = q.limit.clamp(1, MAX_RESULTS);
    let quoted: Vec<String> = terms
        .iter()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    let conn = db.lock();
    if !schema_present(&conn) {
        return Ok(TxResult {
            hits: Vec::new(),
            match_mode: "and",
            omissions: vec!["ledger_transcripts_empty".into()],
        });
    }
    let mut mode = "and";
    let mut hits = run_search(&conn, q, &quoted.join(" "), limit, budget)?;
    if hits.is_empty() && quoted.len() > 1 {
        mode = "or";
        hits = run_search(&conn, q, &quoted.join(" OR "), limit, budget)?;
    }
    Ok(TxResult {
        hits,
        match_mode: mode,
        omissions: Vec::new(),
    })
}

fn run_search(
    conn: &Connection,
    q: &TxQuery,
    match_query: &str,
    limit: usize,
    budget: &WorkBudget,
) -> Result<Vec<TxHit>, String> {
    let placeholders = (0..q.scopes.len())
        .map(|i| format!("?{}", i + 5))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT m.msg_key, m.revision, snippet(ledger_tx_fts,0,'','','…',{SNIPPET_TOKENS}), bm25(ledger_tx_fts)
         FROM ledger_tx_fts
         JOIN ledger_tx_chunks c ON c.chunk_id=ledger_tx_fts.rowid
         JOIN ledger_tx_messages m ON m.msg_key=c.msg_key AND m.revision=c.revision
         JOIN ledger_tx_conversations v ON v.conv_key=m.conv_key
         WHERE ledger_tx_fts MATCH ?1 AND m.state='current' AND v.state='active'
           AND (?2 IS NULL OR v.provider=?2) AND (?3 IS NULL OR m.role=?3)
           AND (?4=0 OR m.on_current_path=1)
           AND v.scope IN ({placeholders})
         ORDER BY bm25(ledger_tx_fts) LIMIT {}",
        limit * 6
    );
    let mut values: Vec<rusqlite::types::Value> = vec![
        match_query.to_string().into(),
        q.provider.clone().into(),
        q.role.clone().into(),
        (q.current_path_only as i64).into(),
    ];
    values.extend(q.scopes.iter().cloned().map(Into::into));
    let mut statement = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(values), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, f64>(3)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let mut seen = BTreeSet::new();
    let mut hits = Vec::new();
    for (key, revision, snippet, score) in rows {
        budget.check()?;
        if !seen.insert((key.clone(), revision.clone())) {
            continue;
        }
        let hit = conn
            .query_row(
                "SELECT m.conv_key,m.provider_msg_id,m.role,m.created_ms,m.on_current_path,
                        v.scope,v.provider,v.provider_conv_id,v.title,v.branch,v.workspace
                 FROM ledger_tx_messages m JOIN ledger_tx_conversations v ON v.conv_key=m.conv_key
                 WHERE m.msg_key=?1 AND m.revision=?2
                   AND (?3 IS NULL OR m.created_ms>=?3) AND (?4 IS NULL OR m.created_ms<=?4)",
                params![key, revision, q.since_ms, q.until_ms],
                |r| {
                    Ok(TxHit {
                        conv_key: r.get(0)?,
                        msg_key: key.clone(),
                        revision: revision.clone(),
                        provider_msg_id: r.get(1)?,
                        role: r.get(2)?,
                        created_ms: r.get(3)?,
                        on_current_path: r.get::<_, i64>(4)? != 0,
                        scope: r.get(5)?,
                        provider: r.get(6)?,
                        provider_conv_id: r.get(7)?,
                        title: r.get(8)?,
                        branch: r.get(9)?,
                        workspace: r.get(10)?,
                        snippet: snippet.clone(),
                        score: -score,
                        citations: Vec::new(),
                    })
                },
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some(mut hit) = hit else { continue };
        hit.citations = citations(conn, &key, &revision)?;
        hits.push(hit);
        if hits.len() >= limit {
            break;
        }
    }
    Ok(hits)
}

fn citations(conn: &Connection, key: &str, revision: &str) -> Result<Vec<Citation>, String> {
    let mut statement = conn
        .prepare(
            "SELECT device,source_id,rel_path,byte_start,byte_end,locator,raw_sha256 FROM ledger_tx_seen
             WHERE msg_key=?1 AND revision=?2 ORDER BY device,source_id LIMIT 16",
        )
        .map_err(|e| e.to_string())?;
    let rows = statement
        .query_map(params![key, revision], |r| {
            Ok(Citation {
                device: r.get(0)?,
                source_id: r.get(1)?,
                rel_path: r.get(2)?,
                byte_start: r.get(3)?,
                byte_end: r.get(4)?,
                locator: r.get(5)?,
                raw_sha256: r.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TxMessage {
    pub msg_key: String,
    pub revision: String,
    pub provider_msg_id: String,
    pub parent_msg_id: Option<String>,
    pub role: String,
    pub kind: String,
    pub ordinal: i64,
    pub created_ms: i64,
    pub on_current_path: bool,
    pub text: String,
    pub truncated: bool,
}

/// The turn around a hit: up to `before`/`after` neighbouring messages of the same conversation
/// (tool events included), in order, restricted to scopes the caller may read. The messages are
/// the canonical stored ones, not the search chunks.
pub fn expand(
    db: &LedgerDb,
    manifest: &Path,
    scopes: &[String],
    msg_key: &str,
    revision: &str,
    before: usize,
    after: usize,
) -> Result<Vec<TxMessage>, String> {
    authorize_scopes(manifest, scopes)?;
    if !is_hex64(msg_key) || !is_hex64(revision) {
        return Err("ledger_transcript_key_invalid".into());
    }
    let (before, after) = (before.min(MAX_EXPAND) as i64, after.min(MAX_EXPAND) as i64);
    let conn = db.lock();
    if !schema_present(&conn) {
        return Ok(Vec::new());
    }
    let placeholders = (0..scopes.len())
        .map(|i| format!("?{}", i + 5))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "WITH anchor AS (SELECT m.conv_key AS ck, m.ordinal AS ord FROM ledger_tx_messages m
            JOIN ledger_tx_conversations v ON v.conv_key=m.conv_key
            WHERE m.msg_key=?1 AND m.revision=?2 AND v.state='active' AND v.scope IN ({placeholders}))
         SELECT * FROM (
           SELECT m.msg_key,m.revision,m.provider_msg_id,m.parent_msg_id,m.role,m.kind,m.ordinal,
                  m.created_ms,m.on_current_path,m.text,m.truncated
           FROM ledger_tx_messages m, anchor WHERE m.conv_key=anchor.ck AND m.state='current'
             AND m.ordinal<anchor.ord ORDER BY m.ordinal DESC LIMIT ?3)
         UNION ALL
         SELECT * FROM (
           SELECT m.msg_key,m.revision,m.provider_msg_id,m.parent_msg_id,m.role,m.kind,m.ordinal,
                  m.created_ms,m.on_current_path,m.text,m.truncated
           FROM ledger_tx_messages m, anchor WHERE m.conv_key=anchor.ck AND m.state='current'
             AND m.ordinal>=anchor.ord ORDER BY m.ordinal LIMIT ?4)
         ORDER BY 7"
    );
    let mut values: Vec<rusqlite::types::Value> = vec![
        msg_key.to_string().into(),
        revision.to_string().into(),
        before.into(),
        (after + 1).into(),
    ];
    values.extend(scopes.iter().cloned().map(Into::into));
    let mut statement = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(values), |r| {
            Ok(TxMessage {
                msg_key: r.get(0)?,
                revision: r.get(1)?,
                provider_msg_id: r.get(2)?,
                parent_msg_id: r.get(3)?,
                role: r.get(4)?,
                kind: r.get(5)?,
                ordinal: r.get(6)?,
                created_ms: r.get(7)?,
                on_current_path: r.get::<_, i64>(8)? != 0,
                text: r.get(9)?,
                truncated: r.get::<_, i64>(10)? != 0,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

// ---- status ----

/// What the lane holds and how each source fared. An unavailable or empty source is reported as
/// such, never as corruption; `enabled` mirrors the manifest.
pub fn status(db: &LedgerDb, manifest: &Path) -> Result<serde_json::Value, String> {
    let manifest = load_manifest(manifest);
    let (enabled, manifest_error) = match &manifest {
        Ok(Some(m)) => (m.enabled, None),
        Ok(None) => (false, None),
        Err(e) => (false, Some(e.clone())),
    };
    // Off, or a manifest that cannot be read: nothing stored is described (counts and sources are
    // history metadata too); the stored copy waits for maintenance to purge it.
    if !enabled {
        return Ok(
            serde_json::json!({"enabled":false,"manifestError":manifest_error,
            "stored":false,"conversations":0,"messages":0,"sources":[]}),
        );
    }
    let conn = db.lock();
    if !schema_present(&conn) {
        return Ok(
            serde_json::json!({"enabled":enabled,"manifestError":manifest_error,
            "stored":false,"conversations":0,"messages":0,"sources":[]}),
        );
    }
    let count = |sql: &str| -> Result<i64, String> {
        conn.query_row(sql, [], |r| r.get(0))
            .map_err(|e| e.to_string())
    };
    let mut statement = conn
        .prepare(
            "SELECT source_id,device,provider,scope,status,detail,files_seen,files_failed,last_scan_ms,
               (SELECT COUNT(*) FROM ledger_tx_files f WHERE f.source_id=s.source_id AND f.state='missing'),
               (SELECT COUNT(*) FROM ledger_tx_files f WHERE f.source_id=s.source_id AND f.state='unsupported')
             FROM ledger_tx_sources s ORDER BY source_id",
        )
        .map_err(|e| e.to_string())?;
    let sources = statement
        .query_map([], |r| {
            Ok(serde_json::json!({"sourceId":r.get::<_, String>(0)?,"device":r.get::<_, String>(1)?,
                "provider":r.get::<_, String>(2)?,"scope":r.get::<_, String>(3)?,
                "status":r.get::<_, String>(4)?,"detail":r.get::<_, String>(5)?,
                "filesSeen":r.get::<_, i64>(6)?,"filesFailed":r.get::<_, i64>(7)?,
                "lastScanMs":r.get::<_, i64>(8)?,"filesMissing":r.get::<_, i64>(9)?,
                "filesUnsupported":r.get::<_, i64>(10)?}))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(
        serde_json::json!({"enabled":enabled,"manifestError":manifest_error,"stored":true,
        "conversations":count("SELECT COUNT(*) FROM ledger_tx_conversations")?,
        "messages":count("SELECT COUNT(*) FROM ledger_tx_messages WHERE state='current'")?,
        "revisions":count("SELECT COUNT(*) FROM ledger_tx_messages")?,
        "sources":sources}),
    )
}

// ---- exact-source verification ----

/// Re-read the cited source and confirm the message is still exactly what the index says:
/// a Codex citation re-hashes its byte range, a ChatGPT one re-reads the cited node. A changed,
/// moved or missing source returns `Ok(false)`; the caller must not present such text as a quote.
pub fn verify_citation(manifest: &Manifest, source_id: &str, c: &Citation) -> Result<bool, String> {
    let source = manifest
        .sources
        .iter()
        .find(|s| s.id == source_id)
        .ok_or("ledger_transcript_source_unknown")?;
    let relative = Path::new(&c.rel_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|p| !matches!(p, std::path::Component::Normal(_)))
    {
        return Err("ledger_transcript_path_denied".into());
    }
    let root =
        std::fs::canonicalize(&source.root).map_err(|_| "ledger_transcript_source_unavailable")?;
    let path = root.join(relative);
    let metadata =
        std::fs::symlink_metadata(&path).map_err(|_| "ledger_transcript_source_unavailable")?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("ledger_transcript_path_denied".into());
    }
    if source.provider == "codex" {
        if c.byte_end <= c.byte_start || c.byte_end - c.byte_start > 64 * 1024 * 1024 {
            return Ok(false);
        }
        let mut file = File::open(&path).map_err(|_| "ledger_transcript_source_unavailable")?;
        file.seek(SeekFrom::Start(c.byte_start as u64))
            .map_err(|_| "ledger_transcript_source_unavailable")?;
        let mut bytes = vec![0u8; (c.byte_end - c.byte_start) as usize];
        if file.read_exact(&mut bytes).is_err() {
            return Ok(false);
        }
        return Ok(sha256_bytes(&bytes) == c.raw_sha256);
    }
    let Some((conversation, node)) = c.locator.split_once('/') else {
        return Ok(false);
    };
    let mut found = false;
    let mut walk = |value: serde_json::Value| -> Result<(), String> {
        let id = value
            .get("conversation_id")
            .or_else(|| value.get("id"))
            .and_then(serde_json::Value::as_str);
        if id == Some(conversation) {
            found = value
                .get("mapping")
                .and_then(|m| m.get(node))
                .and_then(|n| n.get("message"))
                .map(|m| sha256_bytes(m.to_string().as_bytes()) == c.raw_sha256)
                .unwrap_or(false);
        }
        Ok(())
    };
    each_array_element(&path, &mut walk)?;
    Ok(found)
}

// ---- default-owner entry points for callers outside the resident ----

/// Search on the canonical Ledger index (a second, reader connection). For the CLI and the
/// resident's operation handler; the lane stays in the Ledger owner's storage.
pub fn search_default(q: &TxQuery, budget: &WorkBudget) -> Result<TxResult, String> {
    let db = LedgerDb::open_default()?;
    search(&db, &manifest_path(), q, budget)
}

/// Erase a conversation through the canonical stores (index plus durable catalog tombstone).
pub fn erase_conversation_default(conv_key: &str) -> Result<usize, String> {
    let db = LedgerDb::open_default()?;
    let catalog =
        ContextCatalog::open(crate::catalog::default_catalog_path().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    erase_conversation(&db, &catalog, conv_key)
}

/// Erase everything one source contributed and keep it excluded, through the canonical stores.
pub fn erase_source_default(source_id: &str) -> Result<(), String> {
    let db = LedgerDb::open_default()?;
    let catalog =
        ContextCatalog::open(crate::catalog::default_catalog_path().map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    erase_source(&db, &catalog, source_id)
}

/// Lane status from the canonical index and the installed manifest.
pub fn status_default() -> Result<serde_json::Value, String> {
    let db = LedgerDb::open_default()?;
    status(&db, &manifest_path())
}
