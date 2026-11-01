//! P3 acquisition chain, PostRight's side (docs/contracts/p3-acquisition-gates.md). Schema v38 of the channel store.
//! Nothing here calls ScrapeRight or any platform: a ScrapeRight result is IMPORTED (fixture or a live run Adrian
//! approved). What this module owns:
//!
//! - `acquisition.approve` records Adrian's explicit approval of one live run (creator, item count, window).
//!   `acquisition.request` writes the stage 1 request and is refused (`approval_required`, `approval_mismatch`)
//!   without a matching approval. Cookies other than `none` are refused (`cookies_forbidden`). Foreign scope is
//!   refused before anything is read or written.
//! - `acquisition.import` validates and stores a catalogue result. Counter provenance is stored per counter. The
//!   `permitted_*` flags are computed from the method, never taken from the result: official API and user export
//!   rows may be correlated with plays and may feed medians; `public_page_text` feeds research medians only and is
//!   never correlated (decision 1); any other method is stored as forbidden for both.
//! - `acquisition.media_import` stores media records (analysis only, hash, `retain_until = downloaded + 30 days`).
//! - `acquisition.transcript_import` stores a word-timed transcript; unordered or out-of-range timing is
//!   `timing_export_malformed`. `acquisition.label_put` stores the eight labels; a quote that is not in the
//!   transcript span is `label_ungrounded` (so a caption or on-screen text cannot become a label).
//! - `acquisition.correlate` writes a correlation row only for a permitted counter (`source_forbidden` otherwise).
//! - `acquisition.playbook` writes the per-niche playbook. Medians need at least 10 items (`ACQ_MIN_SAMPLE`);
//!   below that the playbook says `insufficient_evidence` and holds no median.
//! - `acquisition.prune` deletes raw media past retention and writes a receipt; metadata and the hash stay.
//! - `acquisition.media_use` refuses any republication of analysis media (`media_not_licensed`).
//! - `acquisition.visual_extract` reports the blocked `visual_input` capability.
//!
//! Raw media lives under `<store root>/analysis-media/<venture>/<channel>/<account>/` (`ANALYSIS_DIR`). The off-host
//! backup must exclude that directory (backup.rs owns that; see the lane report).
//!
//! Ids: `req.catalogue.<16 hex>` is the first 16 hex digits of the BLAKE3 digest of the canonical JSON of the inputs.
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::protocol::{
    AcqApproveInput, AcqCorrelateInput, AcqCreator, AcqImportInput, AcqLabelPutInput,
    AcqMediaImportInput, AcqMediaUseInput, AcqPlaybookGetInput, AcqPlaybookInput, AcqPruneInput,
    AcqRequestInput, AcqStageRunInput, AcqStageStatusInput, AcqTranscriptImportInput,
    AcqVisualInput, AcqWindow, Scope, ACQ_COUNTER_NAMES, ACQ_LABEL_KEYS, ACQ_METHOD_EXPORT,
    ACQ_METHOD_OFFICIAL, ACQ_METHOD_PUBLIC, ACQ_MIN_SAMPLE, ACQ_PLATFORMS, ACQ_RETENTION_DAYS,
    ACQ_STAGES,
};
use rightkit_db::rusqlite::{params, Connection, OptionalExtension};
use rightkit_db::Migration;
use serde::de::DeserializeOwned;
use serde_json::{json, Map, Value};

use crate::host::{engine_name, open_channels};
use crate::Fail;
use rightkit_social_publish::calendar_time as time;

/// Methods that write.
pub const WRITE_METHODS: &[&str] = &[
    "acquisition.approve",
    "acquisition.request",
    "acquisition.import",
    "acquisition.media_import",
    "acquisition.transcript_import",
    "acquisition.label_put",
    "acquisition.correlate",
    "acquisition.playbook",
    "acquisition.prune",
    "acquisition.stage_run",
];

/// Every method this module routes.
pub const METHODS: &[&str] = &[
    "acquisition.approve",
    "acquisition.request",
    "acquisition.import",
    "acquisition.media_import",
    "acquisition.transcript_import",
    "acquisition.label_put",
    "acquisition.correlate",
    "acquisition.playbook",
    "acquisition.playbook_get",
    "acquisition.prune",
    "acquisition.media_use",
    "acquisition.visual_extract",
    "acquisition.stage_run",
    "acquisition.stage_status",
];

/// Raw reference media root, under the store root.
pub const ANALYSIS_DIR: &str = "analysis-media";

const SCHEMA_V38: &str = "
CREATE TABLE acquisition_approvals (
    approval_id TEXT NOT NULL PRIMARY KEY,
    venture TEXT NOT NULL, channel TEXT NOT NULL, account TEXT NOT NULL,
    platform TEXT NOT NULL, handle TEXT NOT NULL, profile_url TEXT NOT NULL,
    window_from TEXT NOT NULL, window_to TEXT NOT NULL, max_items INTEGER NOT NULL,
    approved_by TEXT NOT NULL, approval_ref TEXT NOT NULL, approved_at TEXT NOT NULL
);
CREATE TABLE acquisition_requests (
    request_id TEXT NOT NULL PRIMARY KEY,
    venture TEXT NOT NULL, channel TEXT NOT NULL, account TEXT NOT NULL,
    platform TEXT NOT NULL, handle TEXT NOT NULL, profile_url TEXT NOT NULL,
    window_from TEXT NOT NULL, window_to TEXT NOT NULL, max_items INTEGER NOT NULL,
    counters TEXT NOT NULL CHECK (counters IN ('none', 'public_page_text')),
    approval_id TEXT NOT NULL REFERENCES acquisition_approvals (approval_id),
    requested_at TEXT NOT NULL
);
CREATE TABLE acquisition_catalogues (
    request_id TEXT NOT NULL PRIMARY KEY REFERENCES acquisition_requests (request_id),
    catalogue_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('ok', 'partial', 'failed')),
    partial INTEGER NOT NULL,
    captured_at TEXT NOT NULL,
    tool_json TEXT NOT NULL,
    errors_json TEXT NOT NULL,
    result_hash TEXT NOT NULL,
    imported_at TEXT NOT NULL
);
CREATE TABLE acquisition_items (
    venture TEXT NOT NULL, channel TEXT NOT NULL, account TEXT NOT NULL,
    platform TEXT NOT NULL, item_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    url TEXT, posted_at TEXT, duration_s REAL,
    status TEXT NOT NULL CHECK (status IN ('ok', 'failed')),
    error_code TEXT,
    PRIMARY KEY (venture, channel, account, platform, item_id)
);
CREATE TABLE acquisition_counters (
    venture TEXT NOT NULL, channel TEXT NOT NULL, account TEXT NOT NULL,
    platform TEXT NOT NULL, item_id TEXT NOT NULL,
    name TEXT NOT NULL, value INTEGER NOT NULL CHECK (value >= 0),
    raw_text TEXT, observed_at TEXT NOT NULL,
    method TEXT NOT NULL, source TEXT NOT NULL, tool TEXT NOT NULL, tool_version TEXT,
    permitted_correlation INTEGER NOT NULL, permitted_median INTEGER NOT NULL,
    PRIMARY KEY (venture, channel, account, platform, item_id, name, method, observed_at)
);
CREATE TABLE acquisition_media (
    media_id TEXT NOT NULL UNIQUE,
    venture TEXT NOT NULL, channel TEXT NOT NULL, account TEXT NOT NULL,
    platform TEXT NOT NULL, item_id TEXT NOT NULL,
    blake3 TEXT NOT NULL, bytes INTEGER NOT NULL, duration_s REAL NOT NULL,
    media_path TEXT NOT NULL, downloaded_at TEXT NOT NULL, retain_until TEXT NOT NULL,
    usage TEXT NOT NULL CHECK (usage = 'analysis_only'),
    deleted_at TEXT,
    PRIMARY KEY (venture, channel, account, blake3)
);
CREATE TABLE acquisition_media_receipts (
    receipt_id INTEGER PRIMARY KEY AUTOINCREMENT,
    media_id TEXT NOT NULL, blake3 TEXT NOT NULL, deleted_at TEXT NOT NULL, reason TEXT NOT NULL
);
CREATE TABLE acquisition_transcripts (
    venture TEXT NOT NULL, channel TEXT NOT NULL, account TEXT NOT NULL,
    platform TEXT NOT NULL, item_id TEXT NOT NULL,
    transcript_id TEXT NOT NULL, media_blake3 TEXT NOT NULL,
    engine_json TEXT NOT NULL, language TEXT, text TEXT NOT NULL, words_json TEXT NOT NULL, srt TEXT,
    started_at TEXT, finished_at TEXT,
    PRIMARY KEY (venture, channel, account, platform, item_id)
);
CREATE TABLE acquisition_labels (
    venture TEXT NOT NULL, channel TEXT NOT NULL, account TEXT NOT NULL,
    platform TEXT NOT NULL, item_id TEXT NOT NULL,
    labels_json TEXT NOT NULL, labeler_json TEXT NOT NULL, labelled_at TEXT NOT NULL, transcript_id TEXT NOT NULL,
    PRIMARY KEY (venture, channel, account, platform, item_id)
);
CREATE TABLE acquisition_correlations (
    venture TEXT NOT NULL, channel TEXT NOT NULL, account TEXT NOT NULL,
    platform TEXT NOT NULL, item_id TEXT NOT NULL,
    plays INTEGER NOT NULL, plays_provenance_json TEXT NOT NULL, labels_json TEXT NOT NULL, correlated_at TEXT NOT NULL,
    PRIMARY KEY (venture, channel, account, platform, item_id)
);
CREATE TABLE acquisition_playbooks (
    playbook_id TEXT NOT NULL PRIMARY KEY,
    venture TEXT NOT NULL, channel TEXT NOT NULL, account TEXT NOT NULL,
    niche TEXT NOT NULL, request_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('ok', 'insufficient_evidence')),
    sample_size INTEGER NOT NULL, minimum_sample INTEGER NOT NULL,
    medians_json TEXT, caveats_json TEXT NOT NULL, excluded_json TEXT NOT NULL,
    inputs_json TEXT NOT NULL, plays_source_json TEXT NOT NULL, sample_json TEXT NOT NULL,
    written_at TEXT NOT NULL
);
";

/// Schema v38, appended to the channel store's migration list in channels.rs.
pub const MIGRATION_V38: Migration = Migration::sql(38, "acquisition-chain", SCHEMA_V38);

const EPS: f64 = 1e-6;

fn bad(msg: impl Into<String>) -> Fail {
    ("bad_input", msg.into())
}

fn db_err(e: impl std::fmt::Display) -> Fail {
    ("db_error", e.to_string())
}

fn internal(e: impl std::fmt::Display) -> Fail {
    ("internal", e.to_string())
}

fn parse<T: DeserializeOwned>(input: Value, method: &str) -> Result<T, Fail> {
    serde_json::from_value(input).map_err(|e| bad(format!("{method} input: {e}")))
}

fn open_store(root: &Path) -> Result<Connection, Fail> {
    open_channels(root)
}

/// `foreign_scope` unless the scope exactly matches a stored channel. Runs before anything else.
fn require_channel(conn: &Connection, scope: &Scope) -> Result<(), Fail> {
    let known: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM channels WHERE venture = ?1 AND channel = ?2 AND account = ?3",
            params![scope.venture, scope.channel, scope.account],
            |r| r.get(0),
        )
        .map_err(db_err)?;
    if known == 0 {
        return Err((
            "foreign_scope",
            format!(
                "no stored channel for venture={} channel={} account={}",
                scope.venture, scope.channel, scope.account
            ),
        ));
    }
    Ok(())
}

fn segment_ok(raw: &str) -> bool {
    !raw.is_empty()
        && !raw.starts_with('.')
        && !raw
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
}

/// `<root>/analysis-media/<venture>/<channel>/<account>`.
fn analysis_root(root: &Path, scope: &Scope) -> Result<PathBuf, Fail> {
    if ![&scope.venture, &scope.channel, &scope.account]
        .into_iter()
        .all(|s| segment_ok(s))
    {
        return Err(bad("scope names are not safe path segments"));
    }
    Ok(root
        .join(ANALYSIS_DIR)
        .join(&scope.venture)
        .join(&scope.channel)
        .join(&scope.account))
}

// ---------------------------------------------------------------- hashing and canonical JSON

fn canon(v: &Value, out: &mut String) {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).unwrap_or_default());
                out.push(':');
                canon(&map[k], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canon(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// 16 lower-case hex: the first 64 bits of the BLAKE3 digest of the canonical JSON (`rightkit_fs::digest`).
fn hash16(v: &Value) -> String {
    let mut text = String::new();
    canon(v, &mut text);
    rightkit_fs::digest::blake3_bytes(text.as_bytes())[..16].to_string()
}

fn scope_json(s: &Scope) -> Value {
    json!({ "venture": s.venture, "channel": s.channel, "account": s.account })
}

fn run_json(scope: &Scope, creator: &AcqCreator, window: &AcqWindow, max_items: i64) -> Value {
    json!({
        "scope": scope_json(scope),
        "creator": { "platform": creator.platform, "handle": creator.handle, "profileUrl": creator.profile_url },
        "window": { "from": window.from, "to": window.to },
        "maxItems": max_items,
    })
}

// ---------------------------------------------------------------- small readers

fn field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn need<'a>(v: &'a Value, key: &str, what: &str) -> Result<&'a str, Fail> {
    field(v, key)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad(format!("{what}: {key} must be a non-empty string")))
}

fn need_f64(v: &Value, key: &str, what: &str) -> Result<f64, Fail> {
    v.get(key)
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0)
        .ok_or_else(|| bad(format!("{what}: {key} must be a number >= 0")))
}

fn date_secs(date: &str, what: &str) -> Result<i64, Fail> {
    if date.len() != 10 {
        return Err(bad(format!("{what} must be YYYY-MM-DD, got {date}")));
    }
    time::parse_instant(&format!("{date}T00:00:00Z"), what)
}

fn blake3_ok(text: &str) -> bool {
    text.strip_prefix("blake3:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn media_id_of(blake3: &str) -> String {
    format!("media.{}", &blake3["blake3:".len().."blake3:".len() + 16])
}

fn is_permitted_correlation(method: &str) -> bool {
    method == ACQ_METHOD_OFFICIAL || method == ACQ_METHOD_EXPORT
}

fn is_permitted_median(method: &str) -> bool {
    is_permitted_correlation(method) || method == ACQ_METHOD_PUBLIC
}

// ---------------------------------------------------------------- approval and request

fn validate_run(
    creator: &AcqCreator,
    window: &AcqWindow,
    max_items: i64,
    now: i64,
) -> Result<(), Fail> {
    if !ACQ_PLATFORMS.contains(&creator.platform.as_str()) {
        return Err(bad(format!(
            "creator.platform must be one of {ACQ_PLATFORMS:?}"
        )));
    }
    if creator.handle.is_empty() || creator.handle.len() > 100 {
        return Err(bad("creator.handle must be 1 to 100 characters"));
    }
    if !creator.profile_url.starts_with("https://") {
        return Err(bad("creator.profileUrl must be https"));
    }
    let from = date_secs(&window.from, "window.from")?;
    let to = date_secs(&window.to, "window.to")?;
    if from > to {
        return Err(bad("window.from is after window.to"));
    }
    if to > now {
        return Err(bad("window.to may not be in the future"));
    }
    if !(1..=200).contains(&max_items) {
        return Err(bad("maxItems must be 1 to 200"));
    }
    Ok(())
}

fn approve(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: AcqApproveInput = parse(input, "acquisition.approve")?;
    let now = time::now_secs(req.now.as_deref())?;
    validate_run(&req.creator, &req.window, req.max_items, now)?;
    if req.approved_by.trim().is_empty() || req.approval_ref.trim().is_empty() {
        return Err(bad(
            "approved_by and approval_ref are required: say who approved and where",
        ));
    }
    let mut conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    let approval_id = format!(
        "appr.{}",
        hash16(&run_json(
            &req.scope,
            &req.creator,
            &req.window,
            req.max_items
        ))
    );
    let tx = rightkit_db::write_txn(&mut conn).map_err(db_err)?;
    let inserted = tx
        .execute(
            "INSERT OR IGNORE INTO acquisition_approvals (approval_id, venture, channel, account, platform, handle,
             profile_url, window_from, window_to, max_items, approved_by, approval_ref, approved_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                approval_id,
                req.scope.venture,
                req.scope.channel,
                req.scope.account,
                req.creator.platform,
                req.creator.handle,
                req.creator.profile_url,
                req.window.from,
                req.window.to,
                req.max_items,
                req.approved_by,
                req.approval_ref,
                time::format_instant(now)
            ],
        )
        .map_err(db_err)?;
    tx.commit().map_err(db_err)?;
    Ok(json!({ "approval_id": approval_id, "replayed": inserted == 0 }))
}

fn request(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: AcqRequestInput = parse(input, "acquisition.request")?;
    let mut conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    if let Some(c) = req.cookies.as_deref() {
        if c != "none" {
            return Err((
                "cookies_forbidden",
                "cookies must be none: no cookies from any signed-in profile".to_string(),
            ));
        }
    }
    if req.counters != "none" && req.counters != ACQ_METHOD_PUBLIC {
        return Err(bad("counters must be none or public_page_text"));
    }
    let now = time::now_secs(req.now.as_deref())?;
    validate_run(&req.creator, &req.window, req.max_items, now)?;
    let approval_id = req.approval_id.clone().ok_or_else(|| {
        (
            "approval_required",
            "a live acquisition run needs the approver's recorded approval (acquisition.approve) for this creator, item count and window".to_string(),
        )
    })?;
    let approved: Option<(String, String, String, String, String, i64)> = conn
        .query_row(
            "SELECT platform, handle, window_from, window_to, profile_url, max_items FROM acquisition_approvals
             WHERE approval_id = ?1 AND venture = ?2 AND channel = ?3 AND account = ?4",
            params![approval_id, req.scope.venture, req.scope.channel, req.scope.account],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .optional()
        .map_err(db_err)?;
    let Some((platform, handle, from, to, url, max)) = approved else {
        return Err((
            "approval_required",
            format!("no recorded approval {approval_id} in this scope"),
        ));
    };
    if platform != req.creator.platform
        || handle != req.creator.handle
        || url != req.creator.profile_url
        || from != req.window.from
        || to != req.window.to
        || max != req.max_items
    {
        return Err((
            "approval_mismatch",
            "the approval covers a different creator, item count or window than this request"
                .to_string(),
        ));
    }
    let mut inputs = run_json(&req.scope, &req.creator, &req.window, req.max_items);
    if let Some(map) = inputs.as_object_mut() {
        map.insert("counters".to_string(), json!(req.counters));
    }
    let request_id = format!("req.catalogue.{}", hash16(&inputs));
    let tx = rightkit_db::write_txn(&mut conn).map_err(db_err)?;
    let inserted = tx
        .execute(
            "INSERT OR IGNORE INTO acquisition_requests (request_id, venture, channel, account, platform, handle,
             profile_url, window_from, window_to, max_items, counters, approval_id, requested_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                request_id,
                req.scope.venture,
                req.scope.channel,
                req.scope.account,
                req.creator.platform,
                req.creator.handle,
                req.creator.profile_url,
                req.window.from,
                req.window.to,
                req.max_items,
                req.counters,
                approval_id,
                time::format_instant(now)
            ],
        )
        .map_err(db_err)?;
    tx.commit().map_err(db_err)?;
    Ok(json!({
        "request_id": request_id,
        "approval_id": approval_id,
        "replayed": inserted == 0,
        "cookies": "none",
    }))
}

// ---------------------------------------------------------------- catalogue import

struct ItemIn {
    item_id: String,
    url: String,
    posted_at: String,
    duration_s: f64,
    counters: Vec<CounterIn>,
}

struct CounterIn {
    name: String,
    value: i64,
    raw_text: Option<String>,
    observed_at: String,
    method: String,
    source: String,
    tool: String,
    tool_version: Option<String>,
}

fn read_counter(c: &Value, counters_mode: &str) -> Result<CounterIn, Fail> {
    let name = need(c, "name", "counter")?;
    if !ACQ_COUNTER_NAMES.contains(&name) {
        return Err(bad(format!(
            "counter name {name} is not one of {ACQ_COUNTER_NAMES:?}"
        )));
    }
    let value = c
        .get("value")
        .and_then(Value::as_u64)
        .and_then(|v| i64::try_from(v).ok())
        .ok_or_else(|| bad("counter value must be an integer >= 0"))?;
    let prov = c
        .get("provenance")
        .ok_or_else(|| bad("counter provenance is required"))?;
    let method = need(prov, "method", "counter provenance")?.to_string();
    let source = need(prov, "source", "counter provenance")?.to_string();
    let tool = need(prov, "tool", "counter provenance")?.to_string();
    let observed_at = need(c, "observedAt", "counter")?.to_string();
    time::parse_instant(&observed_at, "counter observedAt")?;
    let raw_text = field(c, "rawText").map(str::to_string);
    if method == ACQ_METHOD_PUBLIC {
        if counters_mode != ACQ_METHOD_PUBLIC {
            return Err((
                "counters_not_requested",
                "the request asked for no counters, but the result carries public page counters"
                    .to_string(),
            ));
        }
        if raw_text.as_deref().map_or(true, str::is_empty) {
            return Err(bad("a public_page_text counter keeps its rawText"));
        }
    }
    Ok(CounterIn {
        name: name.to_string(),
        value,
        raw_text,
        observed_at,
        method,
        source,
        tool,
        tool_version: field(prov, "toolVersion").map(str::to_string),
    })
}

fn read_item(v: &Value, counters_mode: &str) -> Result<ItemIn, Fail> {
    let item_id = need(v, "itemId", "item")?.to_string();
    let url = need(v, "url", "item")?.to_string();
    if !url.starts_with("https://") {
        return Err(bad(format!("item {item_id}: url must be https")));
    }
    let posted_at = need(v, "postedAt", "item")?.to_string();
    let duration_s = need_f64(v, "durationS", "item")?;
    let mut counters = Vec::new();
    if let Some(list) = v.get("counters") {
        let list = list
            .as_array()
            .ok_or_else(|| bad("item counters must be an array"))?;
        for c in list {
            counters.push(read_counter(c, counters_mode)?);
        }
    }
    Ok(ItemIn {
        item_id,
        url,
        posted_at,
        duration_s,
        counters,
    })
}

fn import(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: AcqImportInput = parse(input, "acquisition.import")?;
    let mut conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    let now = time::format_instant(time::now_secs(req.now.as_deref())?);
    let stored: Option<(String, String, i64, String)> = conn
        .query_row(
            "SELECT platform, counters, max_items, request_id FROM acquisition_requests
             WHERE request_id = ?1 AND venture = ?2 AND channel = ?3 AND account = ?4",
            params![
                req.request_id,
                req.scope.venture,
                req.scope.channel,
                req.scope.account
            ],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(db_err)?;
    let Some((platform, counters_mode, max_items, _)) = stored else {
        return Err((
            "not_found",
            format!(
                "no acquisition request {} in this scope; record it with an approval first",
                req.request_id
            ),
        ));
    };
    let result = &req.result;
    if field(result, "requestId") != Some(req.request_id.as_str()) {
        return Err(bad("result.requestId does not match request_id"));
    }
    let status = need(result, "status", "result")?.to_string();
    if !["ok", "partial", "failed"].contains(&status.as_str()) {
        return Err(bad("result.status must be ok, partial or failed"));
    }
    let catalogue_id = need(result, "catalogueId", "result")?.to_string();
    let captured_at = need(result, "capturedAt", "result")?.to_string();
    let tool = result
        .get("toolProvenance")
        .cloned()
        .ok_or_else(|| bad("result.toolProvenance is required"))?;
    let errors: Vec<Value> = result
        .get("errors")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if status == "partial" && errors.is_empty() {
        return Err(bad("a partial result must list at least one error"));
    }
    let raw_items = result
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("result.items must be an array"))?;
    if raw_items.len() as i64 > max_items {
        return Err(bad(format!(
            "result has {} items, the request allowed {max_items}",
            raw_items.len()
        )));
    }
    let mut items = Vec::with_capacity(raw_items.len());
    for v in raw_items {
        items.push(read_item(v, &counters_mode)?);
    }
    let mut failed: Vec<(String, String)> = Vec::new();
    for e in &errors {
        let code = need(e, "code", "error")?.to_string();
        if let Some(id) = field(e, "itemId") {
            failed.push((id.to_string(), code));
        }
    }
    let result_hash = hash16(result);

    let tx = rightkit_db::write_txn(&mut conn).map_err(db_err)?;
    let prior: Option<(String, String)> = tx
        .query_row(
            "SELECT status, result_hash FROM acquisition_catalogues WHERE request_id = ?1",
            params![req.request_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(db_err)?;
    if let Some((prior_status, prior_hash)) = prior {
        if prior_hash == result_hash {
            return Ok(json!({
                "request_id": req.request_id, "catalogue_id": catalogue_id, "status": status, "replayed": true
            }));
        }
        if prior_status != "partial" {
            return Err((
                "request_mismatch",
                "a different result for a request whose catalogue is already complete".to_string(),
            ));
        }
    }
    tx.execute(
        "INSERT OR REPLACE INTO acquisition_catalogues (request_id, catalogue_id, status, partial, captured_at,
         tool_json, errors_json, result_hash, imported_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            req.request_id,
            catalogue_id,
            status,
            i64::from(status == "partial"),
            captured_at,
            tool.to_string(),
            Value::Array(errors.clone()).to_string(),
            result_hash,
            now
        ],
    )
    .map_err(db_err)?;
    let (mut new_items, mut new_counters) = (0_i64, 0_i64);
    for item in &items {
        new_items += tx
            .execute(
                "INSERT OR IGNORE INTO acquisition_items (venture, channel, account, platform, item_id, request_id,
                 url, posted_at, duration_s, status) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'ok')",
                params![
                    req.scope.venture,
                    req.scope.channel,
                    req.scope.account,
                    platform,
                    item.item_id,
                    req.request_id,
                    item.url,
                    item.posted_at,
                    item.duration_s
                ],
            )
            .map_err(db_err)? as i64;
        for c in &item.counters {
            new_counters += tx
                .execute(
                    "INSERT OR IGNORE INTO acquisition_counters (venture, channel, account, platform, item_id, name,
                     value, raw_text, observed_at, method, source, tool, tool_version, permitted_correlation,
                     permitted_median) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                    params![
                        req.scope.venture,
                        req.scope.channel,
                        req.scope.account,
                        platform,
                        item.item_id,
                        c.name,
                        c.value,
                        c.raw_text,
                        c.observed_at,
                        c.method,
                        c.source,
                        c.tool,
                        c.tool_version,
                        i64::from(is_permitted_correlation(&c.method)),
                        i64::from(is_permitted_median(&c.method))
                    ],
                )
                .map_err(db_err)? as i64;
        }
    }
    for (item_id, code) in &failed {
        tx.execute(
            "INSERT OR IGNORE INTO acquisition_items (venture, channel, account, platform, item_id, request_id,
             status, error_code) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'failed', ?7)",
            params![req.scope.venture, req.scope.channel, req.scope.account, platform, item_id, req.request_id, code],
        )
        .map_err(db_err)?;
    }
    tx.commit().map_err(db_err)?;
    Ok(json!({
        "request_id": req.request_id,
        "catalogue_id": catalogue_id,
        "status": status,
        "partial": status == "partial",
        "items": items.len(),
        "new_items": new_items,
        "new_counters": new_counters,
        "failed_items": failed.len(),
        "errors": errors,
        "replayed": false,
    }))
}

// ---------------------------------------------------------------- media, retention

fn media_import(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: AcqMediaImportInput = parse(input, "acquisition.media_import")?;
    let mut conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    let _now = time::now_secs(req.now.as_deref())?;
    let base = analysis_root(root, &req.scope)?;
    let tx = rightkit_db::write_txn(&mut conn).map_err(db_err)?;
    let (mut stored, mut duplicates) = (0_i64, 0_i64);
    let mut ids = Vec::new();
    for m in &req.media {
        let item_id = need(m, "itemId", "media")?;
        let blake3 = need(m, "blake3", "media")?;
        if !blake3_ok(blake3) {
            return Err(bad(format!(
                "media {item_id}: blake3 must be blake3: plus 64 lower-case hex"
            )));
        }
        let media_id = media_id_of(blake3);
        if let Some(given) = field(m, "mediaId") {
            if given != media_id {
                return Err(bad(format!(
                    "media {item_id}: mediaId {given} is not {media_id}"
                )));
            }
        }
        let bytes = m
            .get("bytes")
            .and_then(Value::as_u64)
            .and_then(|b| i64::try_from(b).ok())
            .ok_or_else(|| bad("media bytes must be an integer >= 0"))?;
        let duration_s = need_f64(m, "durationS", "media")?;
        if field(m, "usage") != Some("analysis_only") {
            return Err(bad(format!("media {item_id}: usage must be analysis_only")));
        }
        let media_path = need(m, "mediaPath", "media")?;
        let path = Path::new(media_path);
        if !path.is_absolute()
            || path.components().any(|c| matches!(c, Component::ParentDir))
            || !rightkit_fs::is_within(&base, path)
        {
            return Err((
                "media_unsafe",
                format!(
                    "media {item_id}: mediaPath must be an absolute path under {}",
                    base.display()
                ),
            ));
        }
        let downloaded_at = need(m, "downloadedAt", "media")?;
        let downloaded = time::parse_instant(downloaded_at, "media downloadedAt")?;
        let retain_until =
            time::format_instant(downloaded + ACQ_RETENTION_DAYS * 86_400)[..10].to_string();
        let item: Option<(String, Option<f64>)> = tx
            .query_row(
                "SELECT platform, duration_s FROM acquisition_items
                 WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND platform = ?4 AND item_id = ?5
                   AND status = 'ok'",
                params![req.scope.venture, req.scope.channel, req.scope.account, req.platform, item_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(db_err)?;
        let Some((_, catalogue_duration)) = item else {
            return Err((
                "not_found",
                format!("no catalogue item {item_id} on {}", req.platform),
            ));
        };
        if let Some(d) = catalogue_duration {
            if (d - duration_s).abs() > 1.0 {
                return Err((
                    "duration_mismatch",
                    format!("media {item_id}: duration {duration_s}s differs from the catalogue's {d}s by over one second"),
                ));
            }
        }
        let n = tx
            .execute(
                "INSERT OR IGNORE INTO acquisition_media (media_id, venture, channel, account, platform, item_id,
                 blake3, bytes, duration_s, media_path, downloaded_at, retain_until, usage)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'analysis_only')",
                params![
                    media_id,
                    req.scope.venture,
                    req.scope.channel,
                    req.scope.account,
                    req.platform,
                    item_id,
                    blake3,
                    bytes,
                    duration_s,
                    media_path,
                    downloaded_at,
                    retain_until
                ],
            )
            .map_err(db_err)?;
        if n == 0 {
            duplicates += 1;
        } else {
            stored += 1;
        }
        ids.push(media_id);
    }
    tx.commit().map_err(db_err)?;
    Ok(json!({ "media_ids": ids, "stored": stored, "duplicates": duplicates }))
}

fn prune(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: AcqPruneInput = parse(input, "acquisition.prune")?;
    let mut conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    let now_secs = time::now_secs(req.now.as_deref())?;
    let now = time::format_instant(now_secs);
    let today = now[..10].to_string();
    let base = analysis_root(root, &req.scope)?;
    let tx = rightkit_db::write_txn(&mut conn).map_err(db_err)?;
    let due: Vec<(String, String, String)> = {
        let mut stmt = tx
            .prepare(
                "SELECT media_id, blake3, media_path FROM acquisition_media
                 WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND deleted_at IS NULL AND retain_until < ?4
                 ORDER BY media_id",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(
                params![
                    req.scope.venture,
                    req.scope.channel,
                    req.scope.account,
                    today
                ],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(db_err)?);
        }
        out
    };
    let mut receipts = Vec::new();
    let mut files_removed = 0_i64;
    for (media_id, blake3, media_path) in due {
        let path = Path::new(&media_path);
        if !rightkit_fs::is_within(&base, path)
            || path.components().any(|c| matches!(c, Component::ParentDir))
        {
            return Err((
                "media_unsafe",
                format!("{media_path} is outside the analysis root; not deleted"),
            ));
        }
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() => {
                return Err((
                    "media_unsafe",
                    format!("{media_path} is a directory; not deleted"),
                ));
            }
            Ok(_) => {
                fs::remove_file(path)
                    .map_err(|e| ("media_delete_failed", format!("{media_path}: {e}")))?;
                files_removed += 1;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(("media_delete_failed", format!("{media_path}: {e}"))),
        }
        tx.execute(
            "UPDATE acquisition_media SET deleted_at = ?1 WHERE media_id = ?2",
            params![now, media_id],
        )
        .map_err(db_err)?;
        tx.execute(
            "INSERT INTO acquisition_media_receipts (media_id, blake3, deleted_at, reason)
             VALUES (?1, ?2, ?3, 'retention_expired')",
            params![media_id, blake3, now],
        )
        .map_err(db_err)?;
        receipts.push(json!({
            "mediaId": media_id, "blake3": blake3, "deletedAt": now, "reason": "retention_expired"
        }));
    }
    tx.commit().map_err(db_err)?;
    Ok(json!({ "deleted": receipts.len(), "files_removed": files_removed, "receipts": receipts }))
}

fn media_use(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: AcqMediaUseInput = parse(input, "acquisition.media_use")?;
    if !["brief", "genright", "cutright", "export", "publish"].contains(&req.target.as_str()) {
        return Err(bad(
            "target must be brief, genright, cutright, export or publish",
        ));
    }
    let conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    let usage: Option<String> = conn
        .query_row(
            "SELECT usage FROM acquisition_media WHERE media_id = ?1 AND venture = ?2 AND channel = ?3 AND account = ?4",
            params![req.media_id, req.scope.venture, req.scope.channel, req.scope.account],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_err)?;
    match usage {
        None => Err(("not_found", format!("no media record {}", req.media_id))),
        Some(u) => Err((
            "media_not_licensed",
            format!(
                "media {} is {u}: reference media is never offered to {}",
                req.media_id, req.target
            ),
        )),
    }
}

fn visual_extract(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: AcqVisualInput = parse(input, "acquisition.visual_extract")?;
    let conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    Ok(json!({
        "status": "blocked",
        "blocked_capability": "visual_input",
        "stills_received": req.stills.len(),
        "visual_references": [],
        "message": "no image or frame input exists in rightkit-llm; only transcript and caption labels may be claimed",
    }))
}

// ---------------------------------------------------------------- transcripts and labels

fn transcript_import(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: AcqTranscriptImportInput = parse(input, "acquisition.transcript_import")?;
    let mut conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    let result = &req.result;
    let status = need(result, "status", "transcript result")?;
    if status == "failed" {
        let code = field(result, "code").unwrap_or("transcription_failed");
        return Ok(json!({ "status": "failed", "code": code, "stored": false }));
    }
    if status != "ok" {
        return Err(bad("transcript result.status must be ok or failed"));
    }
    let tx = rightkit_db::write_txn(&mut conn).map_err(db_err)?;
    let media: Option<(String, f64)> = tx
        .query_row(
            "SELECT media_id, duration_s FROM acquisition_media
             WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND blake3 = ?4 AND platform = ?5 AND item_id = ?6",
            params![
                req.scope.venture,
                req.scope.channel,
                req.scope.account,
                req.media_blake3,
                req.platform,
                req.item_id
            ],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(db_err)?;
    let Some((media_id, duration)) = media else {
        return Err((
            "not_found",
            format!("no media record for item {} with that hash", req.item_id),
        ));
    };
    let engine = result
        .get("engine")
        .ok_or_else(|| bad("transcript result.engine is required"))?;
    let name = need(engine, "name", "engine")?;
    let version = need(engine, "version", "engine")?;
    let text = need(result, "text", "transcript result")?;
    let words = result
        .get("words")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("transcript result.words must be an array"))?;
    let malformed = |why: &str| -> Fail {
        (
            "timing_export_malformed",
            format!("item {}: {why}", req.item_id),
        )
    };
    if words.is_empty() {
        return Err(malformed("no words"));
    }
    let mut last_start = 0.0_f64;
    for w in words {
        let (Some(start), Some(end)) = (
            w.get("start").and_then(Value::as_f64),
            w.get("end").and_then(Value::as_f64),
        ) else {
            return Err(malformed("a word lacks start or end"));
        };
        if field(w, "text").is_none()
            || start < 0.0
            || end < start
            || start + EPS < last_start
            || end > duration + EPS
        {
            return Err(malformed(
                "words are unordered or outside the media duration",
            ));
        }
        last_start = start;
    }
    let transcript_id = format!("tr.{media_id}.{name}.{version}");
    let n = tx
        .execute(
            "INSERT OR IGNORE INTO acquisition_transcripts (venture, channel, account, platform, item_id,
             transcript_id, media_blake3, engine_json, language, text, words_json, srt, started_at, finished_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                req.scope.venture,
                req.scope.channel,
                req.scope.account,
                req.platform,
                req.item_id,
                transcript_id,
                req.media_blake3,
                engine.to_string(),
                field(result, "language"),
                text,
                Value::Array(words.clone()).to_string(),
                field(result, "srt"),
                field(result, "startedAt"),
                field(result, "finishedAt")
            ],
        )
        .map_err(db_err)?;
    tx.commit().map_err(db_err)?;
    Ok(
        json!({ "status": "ok", "transcript_id": transcript_id, "stored": n == 1, "replayed": n == 0 }),
    )
}

fn label_put(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: AcqLabelPutInput = parse(input, "acquisition.label_put")?;
    let mut conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    let now = time::format_instant(time::now_secs(req.now.as_deref())?);
    let labels = req
        .labels
        .as_object()
        .ok_or_else(|| bad("labels must be an object"))?;
    if labels.len() != ACQ_LABEL_KEYS.len()
        || !ACQ_LABEL_KEYS.iter().all(|k| labels.contains_key(*k))
    {
        return Err(bad(format!(
            "labels must hold exactly the eight keys {ACQ_LABEL_KEYS:?}"
        )));
    }
    if field(&req.labeler, "model").is_none()
        || field(&req.labeler, "promptVersion").is_none()
        || req.labeler.get("forcedTool") != Some(&Value::Bool(true))
    {
        return Err(bad(
            "labeler needs model, promptVersion and forcedTool: true",
        ));
    }
    let tx = rightkit_db::write_txn(&mut conn).map_err(db_err)?;
    let transcript: Option<(String, String, String)> = tx
        .query_row(
            "SELECT transcript_id, text, words_json FROM acquisition_transcripts
             WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND platform = ?4 AND item_id = ?5",
            params![req.scope.venture, req.scope.channel, req.scope.account, req.platform, req.item_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(db_err)?;
    let Some((transcript_id, text, words_json)) = transcript else {
        return Err((
            "not_found",
            format!("no transcript for item {}", req.item_id),
        ));
    };
    let duration: f64 = tx
        .query_row(
            "SELECT duration_s FROM acquisition_items
             WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND platform = ?4 AND item_id = ?5",
            params![req.scope.venture, req.scope.channel, req.scope.account, req.platform, req.item_id],
            |r| r.get(0),
        )
        .map_err(db_err)?;
    let words: Vec<Value> = serde_json::from_str(&words_json).map_err(internal)?;
    for key in ACQ_LABEL_KEYS {
        let label = &labels[key];
        let ungrounded =
            |why: &str| -> Fail { ("label_ungrounded", format!("label {key}: {why}")) };
        let value = field(label, "value").unwrap_or("");
        let quote = field(label, "quote").unwrap_or("");
        if value.is_empty() || value.chars().count() > 80 {
            return Err(bad(format!(
                "label {key}: value must be 1 to 80 characters"
            )));
        }
        if quote.is_empty() || quote.chars().count() > 200 {
            return Err(ungrounded("quote must be 1 to 200 characters"));
        }
        let span = label
            .get("span")
            .ok_or_else(|| bad(format!("label {key}: span is required")))?;
        let (Some(start), Some(end)) = (
            span.get("startS").and_then(Value::as_f64),
            span.get("endS").and_then(Value::as_f64),
        ) else {
            return Err(bad(format!("label {key}: span needs startS and endS")));
        };
        if start < 0.0 || end < start || end > duration + EPS {
            return Err(bad(format!("label {key}: span is outside the duration")));
        }
        let in_span: Vec<&str> = words
            .iter()
            .filter(|w| {
                w.get("start")
                    .and_then(Value::as_f64)
                    .is_some_and(|s| s + EPS >= start)
                    && w.get("end")
                        .and_then(Value::as_f64)
                        .is_some_and(|e| e <= end + EPS)
            })
            .filter_map(|w| field(w, "text"))
            .collect();
        if !text.contains(quote) || !in_span.join(" ").contains(quote) {
            return Err(ungrounded(
                "quote is not an exact substring of the transcript within the span",
            ));
        }
    }
    let labels_json = req.labels.to_string();
    let existing: Option<String> = tx
        .query_row(
            "SELECT labels_json FROM acquisition_labels
             WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND platform = ?4 AND item_id = ?5",
            params![req.scope.venture, req.scope.channel, req.scope.account, req.platform, req.item_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_err)?;
    if let Some(prior) = existing {
        let same =
            serde_json::from_str::<Value>(&prior).is_ok_and(|p| hash16(&p) == hash16(&req.labels));
        return if same {
            Ok(json!({ "item_id": req.item_id, "replayed": true }))
        } else {
            Err((
                "request_mismatch",
                "labels for this item are already stored and immutable".to_string(),
            ))
        };
    }
    tx.execute(
        "INSERT INTO acquisition_labels (venture, channel, account, platform, item_id, labels_json, labeler_json,
         labelled_at, transcript_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            req.scope.venture,
            req.scope.channel,
            req.scope.account,
            req.platform,
            req.item_id,
            labels_json,
            req.labeler.to_string(),
            now,
            transcript_id
        ],
    )
    .map_err(db_err)?;
    tx.commit().map_err(db_err)?;
    Ok(json!({ "item_id": req.item_id, "transcript_id": transcript_id, "replayed": false }))
}

/// The latest plays counter of an item, among those with `flag_column` set; returns (value, method, source, observed_at).
type Plays = (i64, String, String, String);

fn plays_of(
    conn: &Connection,
    scope: &Scope,
    platform: &str,
    item_id: &str,
    flag_column: &str,
) -> Result<Option<Plays>, Fail> {
    let sql = format!(
        "SELECT value, method, source, observed_at FROM acquisition_counters
         WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND platform = ?4 AND item_id = ?5
           AND name = 'plays' AND {flag_column} = 1 ORDER BY observed_at DESC LIMIT 1"
    );
    conn.query_row(
        &sql,
        params![
            scope.venture,
            scope.channel,
            scope.account,
            platform,
            item_id
        ],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )
    .optional()
    .map_err(db_err)
}

fn any_plays(
    conn: &Connection,
    scope: &Scope,
    platform: &str,
    item_id: &str,
) -> Result<bool, Fail> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM acquisition_counters
             WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND platform = ?4 AND item_id = ?5 AND name = 'plays'",
            params![scope.venture, scope.channel, scope.account, platform, item_id],
            |r| r.get(0),
        )
        .map_err(db_err)?;
    Ok(n > 0)
}

fn labels_of(
    conn: &Connection,
    scope: &Scope,
    platform: &str,
    item_id: &str,
) -> Result<Option<Value>, Fail> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT labels_json FROM acquisition_labels
             WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND platform = ?4 AND item_id = ?5",
            params![scope.venture, scope.channel, scope.account, platform, item_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_err)?;
    raw.map(|s| serde_json::from_str(&s).map_err(internal))
        .transpose()
}

fn correlate(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: AcqCorrelateInput = parse(input, "acquisition.correlate")?;
    let mut conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    let now = time::format_instant(time::now_secs(req.now.as_deref())?);
    let Some((plays, method, source, observed_at)) = plays_of(
        &conn,
        &req.scope,
        &req.platform,
        &req.item_id,
        "permitted_correlation",
    )?
    else {
        return if any_plays(&conn, &req.scope, &req.platform, &req.item_id)? {
            Err((
                "source_forbidden",
                "the item's plays counters come from a source not permitted for correlation"
                    .to_string(),
            ))
        } else {
            Err((
                "no_counter",
                format!("item {} has no plays counter", req.item_id),
            ))
        };
    };
    let Some(labels) = labels_of(&conn, &req.scope, &req.platform, &req.item_id)? else {
        return Err(("no_labels", format!("item {} has no labels", req.item_id)));
    };
    let provenance = json!({ "method": method, "source": source, "observedAt": observed_at });
    let tx = rightkit_db::write_txn(&mut conn).map_err(db_err)?;
    let n = tx
        .execute(
            "INSERT OR IGNORE INTO acquisition_correlations (venture, channel, account, platform, item_id, plays,
             plays_provenance_json, labels_json, correlated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                req.scope.venture,
                req.scope.channel,
                req.scope.account,
                req.platform,
                req.item_id,
                plays,
                provenance.to_string(),
                labels.to_string(),
                now
            ],
        )
        .map_err(db_err)?;
    tx.commit().map_err(db_err)?;
    Ok(
        json!({ "item_id": req.item_id, "plays": plays, "playsProvenance": provenance, "replayed": n == 0 }),
    )
}

// ---------------------------------------------------------------- medians and the playbook

fn number(x: f64) -> Value {
    if x.fract() == 0.0 && x.abs() < 9.0e15 {
        json!(x as i64)
    } else {
        json!(x)
    }
}

/// The median writer: refuses fewer than `ACQ_MIN_SAMPLE` values (`insufficient_sample`). Even counts average the
/// two middle values.
fn write_median(values: &[i64], minimum: i64) -> Result<Value, Fail> {
    if (values.len() as i64) < minimum.max(ACQ_MIN_SAMPLE) {
        return Err((
            "insufficient_sample",
            format!(
                "{} items with permitted counters; a median needs at least {}",
                values.len(),
                minimum.max(ACQ_MIN_SAMPLE)
            ),
        ));
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    let mid = if n % 2 == 1 {
        sorted[n / 2] as f64
    } else {
        (sorted[n / 2 - 1] as f64 + sorted[n / 2] as f64) / 2.0
    };
    Ok(number(mid))
}

fn playbook(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: AcqPlaybookInput = parse(input, "acquisition.playbook")?;
    let mut conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    let now = time::format_instant(time::now_secs(req.now.as_deref())?);
    if req.niche.trim().is_empty() {
        return Err(bad("niche is required"));
    }
    let minimum = req.minimum_sample.unwrap_or(ACQ_MIN_SAMPLE);
    if minimum < ACQ_MIN_SAMPLE {
        return Err(bad(format!(
            "minimum_sample may not be below {ACQ_MIN_SAMPLE}"
        )));
    }
    let catalogue: Option<(String, String, String, String, i64, String, String)> = conn
        .query_row(
            "SELECT c.catalogue_id, c.status, c.errors_json, r.handle, r.max_items, r.window_from, r.window_to
             FROM acquisition_catalogues c JOIN acquisition_requests r ON r.request_id = c.request_id
             WHERE c.request_id = ?1 AND r.venture = ?2 AND r.channel = ?3 AND r.account = ?4",
            params![req.request_id, req.scope.venture, req.scope.channel, req.scope.account],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        )
        .optional()
        .map_err(db_err)?;
    let Some((catalogue_id, cat_status, errors_json, handle, max_items, from, to)) = catalogue
    else {
        return Err((
            "not_found",
            format!("no imported catalogue for request {}", req.request_id),
        ));
    };
    let items: Vec<(String, String, String, Option<String>)> = {
        let mut stmt = conn
            .prepare(
                "SELECT platform, item_id, status, error_code FROM acquisition_items
                 WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND request_id = ?4
                 ORDER BY platform, item_id",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(
                params![
                    req.scope.venture,
                    req.scope.channel,
                    req.scope.account,
                    req.request_id
                ],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(db_err)?);
        }
        out
    };

    let mut sample: Vec<Value> = Vec::new();
    let mut plays_all: Vec<i64> = Vec::new();
    let mut excluded: Vec<Value> = Vec::new();
    let mut methods: Vec<String> = Vec::new();
    let mut last_observed = String::new();
    for (platform, item_id, status, error_code) in &items {
        let mut exclude =
            |reason: &str| excluded.push(json!({ "itemId": item_id, "reason": reason }));
        if status != "ok" {
            exclude(error_code.as_deref().unwrap_or("item_failed"));
            continue;
        }
        let Some(labels) = labels_of(&conn, &req.scope, platform, item_id)? else {
            exclude("no_labels");
            continue;
        };
        let Some((plays, method, source, observed_at)) =
            plays_of(&conn, &req.scope, platform, item_id, "permitted_median")?
        else {
            exclude(if any_plays(&conn, &req.scope, platform, item_id)? {
                "source_forbidden"
            } else {
                "no_counter"
            });
            continue;
        };
        let mut flat = Map::new();
        if let Some(map) = labels.as_object() {
            for key in ACQ_LABEL_KEYS {
                flat.insert(key.to_string(), map[key]["value"].clone());
            }
        }
        if observed_at > last_observed {
            last_observed = observed_at.clone();
        }
        if !methods.contains(&method) {
            methods.push(method.clone());
        }
        plays_all.push(plays);
        sample.push(json!({
            "itemId": item_id, "platform": platform, "plays": plays,
            "method": method, "source": source, "observedAt": observed_at,
            "labels": Value::Object(flat),
        }));
    }
    let sample_size = sample.len() as i64;
    let mut caveats = vec![format!(
        "sample of {sample_size} items from one creator ({handle}), window {from} to {to}"
    )];
    if cat_status == "partial" {
        let codes: Vec<String> = serde_json::from_str::<Vec<Value>>(&errors_json)
            .unwrap_or_default()
            .iter()
            .filter_map(|e| field(e, "code").map(str::to_string))
            .collect();
        caveats.push(format!(
            "catalogue is partial ({}): {} of up to {max_items} items were fetched",
            codes.join(", "),
            items.len()
        ));
    }
    if methods.iter().any(|m| m == ACQ_METHOD_PUBLIC) {
        caveats.push("plays include public page counters read logged-out: research medians only, never correlated".to_string());
    }

    let medians = match write_median(&plays_all, minimum) {
        Ok(overall) => {
            let mut by_label = Map::new();
            for key in ACQ_LABEL_KEYS {
                let mut groups: std::collections::BTreeMap<String, Vec<i64>> =
                    std::collections::BTreeMap::new();
                for s in &sample {
                    if let (Some(v), Some(p)) = (field(&s["labels"], key), s["plays"].as_i64()) {
                        groups.entry(v.to_string()).or_default().push(p);
                    }
                }
                let mut out = Map::new();
                for (value, plays) in groups {
                    let entry = match write_median(&plays, minimum) {
                        Ok(m) => json!({ "count": plays.len(), "median_plays": m }),
                        Err(_) => {
                            json!({ "count": plays.len(), "median_plays": null, "note": "below the minimum sample" })
                        }
                    };
                    out.insert(value, entry);
                }
                by_label.insert(key.to_string(), Value::Object(out));
            }
            Some(json!({ "overall_plays": overall, "by_label": Value::Object(by_label) }))
        }
        Err(e) => {
            if req.require_medians {
                return Err(e);
            }
            caveats.push(format!(
                "insufficient evidence: {sample_size} items, a median needs at least {minimum}"
            ));
            None
        }
    };
    let status = if medians.is_some() {
        "ok"
    } else {
        "insufficient_evidence"
    };
    let kind = if methods.iter().any(|m| m == ACQ_METHOD_PUBLIC) {
        "public_page_text"
    } else if methods.iter().all(|m| m == ACQ_METHOD_EXPORT) && !methods.is_empty() {
        "user_export"
    } else {
        "official_api"
    };
    let plays_source = json!({ "kind": kind, "ref": req.request_id, "retrievedAt": last_observed });
    let inputs = json!({
        "catalogue": catalogue_id,
        "request": req.request_id,
        "items": sample.iter().filter_map(|s| s["itemId"].as_str()).collect::<Vec<_>>(),
    });
    let playbook_id = format!(
        "pb.{}",
        hash16(&json!({
            "scope": scope_json(&req.scope), "request": req.request_id, "niche": req.niche,
            "minimum": minimum, "sample": sample,
        }))
    );
    let tx = rightkit_db::write_txn(&mut conn).map_err(db_err)?;
    let n = tx
        .execute(
            "INSERT OR IGNORE INTO acquisition_playbooks (playbook_id, venture, channel, account, niche, request_id,
             status, sample_size, minimum_sample, medians_json, caveats_json, excluded_json, inputs_json,
             plays_source_json, sample_json, written_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                playbook_id,
                req.scope.venture,
                req.scope.channel,
                req.scope.account,
                req.niche,
                req.request_id,
                status,
                sample_size,
                minimum,
                medians.as_ref().map(Value::to_string),
                Value::from(caveats.clone()).to_string(),
                Value::Array(excluded.clone()).to_string(),
                inputs.to_string(),
                plays_source.to_string(),
                Value::Array(sample.clone()).to_string(),
                now
            ],
        )
        .map_err(db_err)?;
    tx.commit().map_err(db_err)?;
    let mut out = json!({
        "playbook_id": playbook_id,
        "status": status,
        "scope": scope_json(&req.scope),
        "niche": req.niche,
        "sampleSize": sample_size,
        "minimumSample": minimum,
        "caveats": caveats,
        "excluded": excluded,
        "inputs": inputs,
        "playsSource": plays_source,
        "sample": sample,
        "replayed": n == 0,
    });
    if let (Some(m), Some(map)) = (medians, out.as_object_mut()) {
        map.insert("medians".to_string(), m);
    } else if let Some(map) = out.as_object_mut() {
        map.insert("summary".to_string(), json!("insufficient evidence"));
    }
    Ok(out)
}

fn playbook_get(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: AcqPlaybookGetInput = parse(input, "acquisition.playbook_get")?;
    let conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    let row: Option<(String, String, i64, i64, Option<String>, String, String, String, String, String)> = conn
        .query_row(
            "SELECT niche, status, sample_size, minimum_sample, medians_json, caveats_json, excluded_json,
             inputs_json, plays_source_json, sample_json FROM acquisition_playbooks
             WHERE playbook_id = ?1 AND venture = ?2 AND channel = ?3 AND account = ?4",
            params![req.playbook_id, req.scope.venture, req.scope.channel, req.scope.account],
            |r| {
                Ok((
                    r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?,
                    r.get(8)?, r.get(9)?,
                ))
            },
        )
        .optional()
        .map_err(db_err)?;
    let Some((
        niche,
        status,
        sample_size,
        minimum,
        medians,
        caveats,
        excluded,
        inputs,
        source,
        sample,
    )) = row
    else {
        return Err(("not_found", format!("no playbook {}", req.playbook_id)));
    };
    let j = |s: &str| serde_json::from_str::<Value>(s).unwrap_or(Value::Null);
    let mut out = json!({
        "playbook_id": req.playbook_id, "status": status, "niche": niche,
        "sampleSize": sample_size, "minimumSample": minimum,
        "caveats": j(&caveats), "excluded": j(&excluded), "inputs": j(&inputs),
        "playsSource": j(&source), "sample": j(&sample),
    });
    if let (Some(m), Some(map)) = (medians, out.as_object_mut()) {
        map.insert("medians".to_string(), j(&m));
    }
    Ok(out)
}

// ---------------------------------------------------------------- stage model (issue 269)
//
// A run is named by its catalogue request id. Stages after creator intake: catalogue, media, transcript, labels,
// correlation, playbook. Every stage has an input hash: the hash of its upstream stage's output hash and the stored
// data it consumes (the catalogue's is the request id and the imported result hash). The stage request id is
// `req.<stage>.<input hash>` (the catalogue stage keeps the request id that names the run). A stage record is
// written once per input hash and never changed (contract I1 to I3, I6). `acquisition.stage_run` skips a stage whose
// record exists, so an unchanged stage does no work; a changed input changes that stage's hash and, through the
// upstream output hash, every later stage's, and nothing before it. Each stage is its own transaction, so a kill
// between stages leaves the finished stages recorded and the next run continues at the first missing one.

const SCHEMA_V41: &str = "
CREATE TABLE acquisition_stages (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    venture TEXT NOT NULL, channel TEXT NOT NULL, account TEXT NOT NULL,
    run_id TEXT NOT NULL,
    stage TEXT NOT NULL CHECK (stage IN ('catalogue', 'media', 'transcript', 'labels', 'correlation', 'playbook')),
    request_id TEXT NOT NULL, input_hash TEXT NOT NULL, output_hash TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('ok', 'partial')),
    detail_json TEXT NOT NULL,
    started_at TEXT NOT NULL, finished_at TEXT NOT NULL,
    UNIQUE (venture, channel, account, run_id, stage, input_hash)
);
";

/// Schema v41, appended to the channel store's migration list in channels.rs.
pub const MIGRATION_V41: Migration = Migration::sql(41, "acquisition-stages", SCHEMA_V41);

struct Planned {
    stage: &'static str,
    request_id: String,
    input_hash: String,
    output_hash: String,
    rows: usize,
    blocked: Option<String>,
}

struct Plan {
    stages: Vec<Planned>,
    items: Vec<(String, String)>,
    catalogue_status: String,
    catalogue_errors: Vec<Value>,
}

fn blocked_stage(stage: &'static str, why: &str) -> Planned {
    Planned {
        stage,
        request_id: String::new(),
        input_hash: String::new(),
        output_hash: String::new(),
        rows: 0,
        blocked: Some(why.to_string()),
    }
}

fn stage_index(name: &str, what: &str) -> Result<usize, Fail> {
    ACQ_STAGES
        .iter()
        .position(|s| *s == name)
        .ok_or_else(|| bad(format!("{what} must be one of {ACQ_STAGES:?}")))
}

/// The stages of one run with their current input hashes, computed from stored data only. Stops at the first stage
/// whose input does not exist yet (that stage is `blocked`; later stages are not listed).
fn plan(
    conn: &Connection,
    scope: &Scope,
    run_id: &str,
    niche: Option<&str>,
    minimum: i64,
) -> Result<Plan, Fail> {
    let known: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM acquisition_requests WHERE request_id = ?1 AND venture = ?2 AND channel = ?3 AND account = ?4",
            params![run_id, scope.venture, scope.channel, scope.account],
            |r| r.get(0),
        )
        .map_err(db_err)?;
    if known == 0 {
        return Err((
            "not_found",
            format!("no acquisition request {run_id} in this scope"),
        ));
    }
    let cat: Option<(String, String, String)> = conn
        .query_row(
            "SELECT result_hash, status, errors_json FROM acquisition_catalogues WHERE request_id = ?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(db_err)?;
    let mut stages = Vec::new();
    let Some((cat_hash, cat_status, errors_json)) = cat else {
        stages.push(blocked_stage(
            "catalogue",
            "no catalogue result is imported for this request",
        ));
        return Ok(Plan {
            stages,
            items: Vec::new(),
            catalogue_status: "missing".to_string(),
            catalogue_errors: Vec::new(),
        });
    };
    let catalogue_errors: Vec<Value> = serde_json::from_str(&errors_json).unwrap_or_default();
    stages.push(Planned {
        stage: "catalogue",
        request_id: run_id.to_string(),
        input_hash: hash16(&json!({ "run": run_id, "catalogue": cat_hash })),
        output_hash: cat_hash.clone(),
        rows: 0,
        blocked: None,
    });
    let items: Vec<(String, String)> = {
        let mut stmt = conn
            .prepare(
                "SELECT platform, item_id FROM acquisition_items
                 WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND request_id = ?4 AND status = 'ok'
                 ORDER BY platform, item_id",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(
                params![scope.venture, scope.channel, scope.account, run_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(db_err)?);
        }
        out
    };
    let mut upstream = cat_hash;
    for stage in ["media", "transcript", "labels", "correlation", "playbook"] {
        let mut data: Vec<Value> = Vec::new();
        for (platform, item) in &items {
            match stage {
                "media" => {
                    let mut stmt = conn
                        .prepare(
                            "SELECT blake3 FROM acquisition_media
                             WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND platform = ?4 AND item_id = ?5
                             ORDER BY blake3",
                        )
                        .map_err(db_err)?;
                    let rows = stmt
                        .query_map(
                            params![scope.venture, scope.channel, scope.account, platform, item],
                            |r| r.get::<_, String>(0),
                        )
                        .map_err(db_err)?;
                    for row in rows {
                        data.push(json!([item, row.map_err(db_err)?]));
                    }
                }
                "transcript" => {
                    let row: Option<(String, String)> = conn
                        .query_row(
                            "SELECT transcript_id, media_blake3 FROM acquisition_transcripts
                             WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND platform = ?4 AND item_id = ?5",
                            params![scope.venture, scope.channel, scope.account, platform, item],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .optional()
                        .map_err(db_err)?;
                    if let Some((tid, blake)) = row {
                        data.push(json!([item, tid, blake]));
                    }
                }
                "labels" => {
                    if let Some(labels) = labels_of(conn, scope, platform, item)? {
                        data.push(json!([item, hash16(&labels)]));
                    }
                }
                "correlation" => {
                    if labels_of(conn, scope, platform, item)?.is_some() {
                        if let Some((plays, method, _, _)) =
                            plays_of(conn, scope, platform, item, "permitted_correlation")?
                        {
                            data.push(json!([item, plays, method]));
                        }
                    }
                }
                _ => {
                    if labels_of(conn, scope, platform, item)?.is_some() {
                        if let Some((plays, method, _, _)) =
                            plays_of(conn, scope, platform, item, "permitted_median")?
                        {
                            data.push(json!([item, plays, method]));
                        }
                    }
                }
            }
        }
        let empty_ok = stage == "correlation" || stage == "playbook";
        if data.is_empty() && !empty_ok {
            stages.push(blocked_stage(
                match stage {
                    "media" => "media",
                    "transcript" => "transcript",
                    _ => "labels",
                },
                &format!("no {stage} records are stored for this catalogue's items"),
            ));
            break;
        }
        let rows = data.len();
        let mut data = json!(data);
        if stage == "playbook" {
            let Some(niche) = niche.filter(|n| !n.trim().is_empty()) else {
                stages.push(blocked_stage(
                    "playbook",
                    "a niche is required to run the playbook stage",
                ));
                break;
            };
            data = json!({ "niche": niche, "minimum": minimum, "sample": data });
        }
        let input = json!({ "stage": stage, "scope": scope_json(scope), "run": run_id, "upstream": upstream, "data": data });
        let input_hash = hash16(&input);
        let output_hash = if stage == "playbook" {
            hash16(&json!({ "stage": stage, "input": input_hash }))
        } else {
            hash16(&json!({ "stage": stage, "data": data }))
        };
        let static_name: &'static str = match stage {
            "media" => "media",
            "transcript" => "transcript",
            "labels" => "labels",
            "correlation" => "correlation",
            _ => "playbook",
        };
        stages.push(Planned {
            stage: static_name,
            request_id: format!("req.{stage}.{input_hash}"),
            input_hash,
            output_hash: output_hash.clone(),
            rows,
            blocked: None,
        });
        upstream = output_hash;
    }
    Ok(Plan {
        stages,
        items,
        catalogue_status: cat_status,
        catalogue_errors,
    })
}

fn stage_recorded(
    conn: &Connection,
    scope: &Scope,
    run_id: &str,
    p: &Planned,
) -> Result<bool, Fail> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM acquisition_stages
             WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND run_id = ?4 AND stage = ?5 AND input_hash = ?6",
            params![scope.venture, scope.channel, scope.account, run_id, p.stage, p.input_hash],
            |r| r.get(0),
        )
        .map_err(db_err)?;
    Ok(n > 0)
}

/// Do the work of one stage whose record is missing. Returns the stage status and its detail.
fn exec_stage(
    root: &Path,
    conn: &Connection,
    scope: &Scope,
    run_id: &str,
    plan: &Plan,
    p: &Planned,
    niche: Option<&str>,
    minimum: i64,
    now: &str,
) -> Result<(&'static str, Value), Fail> {
    let mut status = "ok";
    let mut errors: Vec<Value> = Vec::new();
    let mut work = json!({ "rows": p.rows });
    match p.stage {
        "catalogue" => {
            if plan.catalogue_status == "partial" {
                status = "partial";
                errors = plan.catalogue_errors.clone();
            }
            work = json!({ "items": plan.items.len(), "catalogueStatus": plan.catalogue_status });
        }
        "correlation" => {
            let (mut written, mut excluded) = (0_i64, Vec::new());
            for (platform, item) in &plan.items {
                if labels_of(conn, scope, platform, item)?.is_none() {
                    continue;
                }
                let out = correlate(
                    root,
                    json!({ "scope": scope_json(scope), "platform": platform, "item_id": item, "now": now }),
                );
                match out {
                    Ok(_) => written += 1,
                    Err((code, _))
                        if ["source_forbidden", "no_counter", "no_labels"].contains(&code) =>
                    {
                        excluded.push(json!({ "itemId": item, "reason": code }));
                    }
                    Err(e) => return Err(e),
                }
            }
            work = json!({ "rows": written, "excluded": excluded });
        }
        "playbook" => {
            let out = playbook(
                root,
                json!({
                    "scope": scope_json(scope), "request_id": run_id,
                    "niche": niche.unwrap_or_default(), "minimum_sample": minimum, "now": now,
                }),
            )?;
            work = json!({
                "playbookId": out["playbook_id"], "playbookStatus": out["status"], "sampleSize": out["sampleSize"],
            });
        }
        _ => {}
    }
    let detail = json!({
        "scope": scope_json(scope),
        "engine": { "name": engine_name(), "version": env!("CARGO_PKG_VERSION") },
        "requestedAt": now, "startedAt": now, "finishedAt": now,
        "errors": errors,
        "work": work,
    });
    Ok((status, detail))
}

fn stage_run(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: AcqStageRunInput = parse(input, "acquisition.stage_run")?;
    let mut conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    let now = time::format_instant(time::now_secs(req.now.as_deref())?);
    let upto = match req.stage.as_deref() {
        Some(s) => stage_index(s, "stage")?,
        None => ACQ_STAGES.len() - 1,
    };
    let halt = match req.halt_after.as_deref() {
        Some(s) => Some(stage_index(s, "halt_after")?),
        None => None,
    };
    let minimum = req.minimum_sample.unwrap_or(ACQ_MIN_SAMPLE);
    if minimum < ACQ_MIN_SAMPLE {
        return Err(bad(format!(
            "minimum_sample may not be below {ACQ_MIN_SAMPLE}"
        )));
    }
    let plan = plan(
        &conn,
        &req.scope,
        &req.request_id,
        req.niche.as_deref(),
        minimum,
    )?;
    let (mut ran, mut skipped, mut log) = (Vec::new(), Vec::new(), Vec::new());
    let (mut halted, mut stopped) = (false, Value::Null);
    for (idx, p) in plan.stages.iter().enumerate() {
        if idx > upto {
            break;
        }
        if let Some(why) = &p.blocked {
            return Err((
                "stage_blocked",
                format!("{}: {why} (finished stages stay recorded)", p.stage),
            ));
        }
        let action = if stage_recorded(&conn, &req.scope, &req.request_id, p)? {
            skipped.push(p.stage);
            "skipped"
        } else {
            let (status, detail) = exec_stage(
                root,
                &conn,
                &req.scope,
                &req.request_id,
                &plan,
                p,
                req.niche.as_deref(),
                minimum,
                &now,
            )?;
            let tx = rightkit_db::write_txn(&mut conn).map_err(db_err)?;
            tx.execute(
                "INSERT OR IGNORE INTO acquisition_stages (venture, channel, account, run_id, stage, request_id,
                 input_hash, output_hash, status, detail_json, started_at, finished_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
                params![
                    req.scope.venture,
                    req.scope.channel,
                    req.scope.account,
                    req.request_id,
                    p.stage,
                    p.request_id,
                    p.input_hash,
                    p.output_hash,
                    status,
                    detail.to_string(),
                    now
                ],
            )
            .map_err(db_err)?;
            tx.commit().map_err(db_err)?;
            ran.push(p.stage);
            "ran"
        };
        log.push(json!({
            "stage": p.stage, "request_id": p.request_id, "input_hash": p.input_hash,
            "output_hash": p.output_hash, "action": action,
        }));
        if p.stage == "catalogue" && plan.catalogue_status == "partial" && !req.allow_partial {
            stopped = json!("catalogue_partial");
            break;
        }
        if halt == Some(idx) {
            halted = true;
            break;
        }
    }
    Ok(json!({
        "run_id": req.request_id,
        "ran": ran, "skipped": skipped,
        "ran_count": ran.len(), "skipped_count": skipped.len(),
        "halted": halted, "stopped": stopped,
        "stages": log,
        "external_calls": 0,
    }))
}

fn stage_status(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: AcqStageStatusInput = parse(input, "acquisition.stage_status")?;
    let conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    let minimum = req.minimum_sample.unwrap_or(ACQ_MIN_SAMPLE);
    let plan = plan(
        &conn,
        &req.scope,
        &req.request_id,
        req.niche.as_deref(),
        minimum.max(ACQ_MIN_SAMPLE),
    )?;
    let mut out = Vec::new();
    for p in &plan.stages {
        if let Some(why) = &p.blocked {
            out.push(json!({ "stage": p.stage, "state": "blocked", "reason": why }));
            continue;
        }
        let records: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM acquisition_stages
                 WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND run_id = ?4 AND stage = ?5",
                params![req.scope.venture, req.scope.channel, req.scope.account, req.request_id, p.stage],
                |r| r.get(0),
            )
            .map_err(db_err)?;
        let state = if stage_recorded(&conn, &req.scope, &req.request_id, p)? {
            "current"
        } else {
            "pending"
        };
        out.push(json!({
            "stage": p.stage, "state": state, "request_id": p.request_id, "input_hash": p.input_hash,
            "output_hash": p.output_hash, "records": records,
        }));
    }
    Ok(json!({ "run_id": req.request_id, "stages": out }))
}

/// Route one method of this module. Unknown methods are `unknown_method`.
pub fn dispatch(root: &Path, method: &str, input: Value) -> Result<Value, Fail> {
    match method {
        "acquisition.approve" => approve(root, input),
        "acquisition.request" => request(root, input),
        "acquisition.import" => import(root, input),
        "acquisition.media_import" => media_import(root, input),
        "acquisition.transcript_import" => transcript_import(root, input),
        "acquisition.label_put" => label_put(root, input),
        "acquisition.correlate" => correlate(root, input),
        "acquisition.playbook" => playbook(root, input),
        "acquisition.playbook_get" => playbook_get(root, input),
        "acquisition.prune" => prune(root, input),
        "acquisition.media_use" => media_use(root, input),
        "acquisition.visual_extract" => visual_extract(root, input),
        "acquisition.stage_run" => stage_run(root, input),
        "acquisition.stage_status" => stage_status(root, input),
        other => Err(("unknown_method", format!("unknown method: {other}"))),
    }
}
