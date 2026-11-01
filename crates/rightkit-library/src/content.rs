//! Content records: reusable media keyed by venture, channel and account, with stable occurrence ids,
//! per-platform cooldowns, expiry, withdrawal, and caption and CTA variants for experiments. This module
//! makes NO network call and posts nothing. The table is schema v8 of the channel store (one database,
//! one migration list in channels.rs). Every command checks its scope against a stored channel.
//!
//! Derived state: `expired` is never stored. A record is `expired` when the clock (`now`, or the current
//! time) is strictly after `expires_at`. `withdrawn` is stored and final.
//!
//! Cooldown rule: two non-cancelled occurrences of one record on one account and platform conflict when
//! their scheduled instants are less than `cooldown_days` * 86400 seconds apart. Exactly `cooldown_days`
//! apart is allowed. A zero cooldown never conflicts.
//!
//! Expiry rule: an occurrence may be planned when its scheduled instant and the clock are both at or
//! before `expires_at`. Strictly after is `content_expired`.
//!
//! Earliest allowed time: the first instant at or after the requested time that conflicts with no
//! non-cancelled occurrence of the same record on the same platform. It is computed by moving forward past
//! each conflicting occurrence until none remains, so it can be later than a second conflict it passes.
//!
//! Revisions: put, withdraw and add_variant bump the record revision. A stale `expected_revision` is
//! `revision_conflict`. A missing one on an existing record is `revision_required`.
use std::path::Path;

use std::collections::BTreeMap;

use crate::content_protocol::{
    ContentAddVariantInput, ContentAddVariantOutput, ContentCancelOccurrenceInput,
    ContentDeclareMetricInput, ContentDeclareMetricOutput, ContentExperimentSummaryInput,
    ContentExperimentSummaryOutput, ContentGetInput, ContentGetOutput, ContentListInput,
    ContentListOutput, ContentMarkPublishedInput, ContentMediaItem, ContentNextAllowedInput,
    ContentNextAllowedOutput, ContentOccurrence, ContentPlanOccurrenceInput,
    ContentPlanOccurrenceOutput, ContentPutInput, ContentRecord, ContentVariant,
    ContentWithdrawInput, ContentWithdrawOutput, VariantCount, CONTENT_MEDIA_KINDS, CONTENT_STATES,
    CONTENT_VARIANT_KINDS,
};
use rightkit_db::rusqlite::{params, Connection, OptionalExtension, Row};
use rightkit_db::Migration;
use rightkit_social_publish::protocol::{Scope, PLATFORMS};
use serde_json::Value;

use rightkit_content_proof::host::open_channels;
use rightkit_social_publish::calendar_time as time;
use rightkit_social_publish::Fail;

/// Every method this module routes in [`dispatch`].
pub const METHODS: &[&str] = &[
    "content.put",
    "content.get",
    "content.list",
    "content.withdraw",
    "content.add_variant",
    "content.plan_occurrence",
    "content.cancel_occurrence",
    "content.mark_published",
    "content.next_allowed",
    "content.experiment_summary",
    "content.declare_metric",
];

/// Methods that write. A dry run reports these and writes nothing.
pub const WRITE_METHODS: &[&str] = &[
    "content.declare_metric",
    "content.put",
    "content.withdraw",
    "content.add_variant",
    "content.plan_occurrence",
    "content.cancel_occurrence",
    "content.mark_published",
];

const SECS_PER_DAY: i64 = 86_400;

/// Default minimum published occurrences per compared variant, from the plan's twenty-post rule.
const DEFAULT_MIN_OCCURRENCES: i64 = 20;

const SCHEMA_V8: &str = "
CREATE TABLE content_records (
    venture TEXT NOT NULL,
    channel TEXT NOT NULL,
    account TEXT NOT NULL,
    content_id TEXT NOT NULL,
    title TEXT NOT NULL,
    media TEXT NOT NULL,
    base_caption TEXT NOT NULL,
    evergreen INTEGER NOT NULL CHECK (evergreen IN (0, 1)),
    cooldown_days INTEGER NOT NULL CHECK (cooldown_days >= 0),
    expires_at TEXT,
    state TEXT NOT NULL CHECK (state IN ('active', 'withdrawn')),
    withdrawal_reason TEXT,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (venture, channel, account, content_id),
    FOREIGN KEY (venture, channel, account) REFERENCES channels (venture, channel, account),
    CHECK ((state = 'withdrawn') = (withdrawal_reason IS NOT NULL))
);
CREATE TABLE content_variants (
    venture TEXT NOT NULL,
    channel TEXT NOT NULL,
    account TEXT NOT NULL,
    variant_id TEXT NOT NULL,
    content_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('caption', 'cta')),
    text TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (venture, channel, account, variant_id),
    FOREIGN KEY (venture, channel, account, content_id) REFERENCES content_records (venture, channel, account, content_id)
);
CREATE TABLE content_occurrences (
    venture TEXT NOT NULL,
    channel TEXT NOT NULL,
    account TEXT NOT NULL,
    occurrence_id TEXT NOT NULL,
    content_id TEXT NOT NULL,
    platform TEXT NOT NULL,
    calendar_entry_id TEXT,
    caption_variant_id TEXT,
    cta_variant_id TEXT,
    scheduled_at TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('planned', 'published', 'cancelled')),
    published_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (venture, channel, account, occurrence_id),
    FOREIGN KEY (venture, channel, account, content_id) REFERENCES content_records (venture, channel, account, content_id),
    CHECK ((state = 'published') = (published_at IS NOT NULL))
);
CREATE INDEX content_occurrences_by_record ON content_occurrences (venture, channel, account, content_id, platform, scheduled_at);
";

/// Schema v8, appended to the channel store's migration list in channels.rs.
pub const MIGRATION_V8: Migration = Migration::sql(8, "content-records", SCHEMA_V8);

/// Schema v30: the primary metric a record's experiment may be decided on. Declared once per record.
const SCHEMA_V30: &str = "
CREATE TABLE content_primary_metrics (
    venture TEXT NOT NULL,
    channel TEXT NOT NULL,
    account TEXT NOT NULL,
    content_id TEXT NOT NULL,
    metric TEXT NOT NULL,
    window_name TEXT NOT NULL CHECK (window_name IN ('lifetime')),
    declared_at TEXT NOT NULL,
    PRIMARY KEY (venture, channel, account, content_id),
    FOREIGN KEY (venture, channel, account, content_id) REFERENCES content_records (venture, channel, account, content_id)
);
";

/// Schema v30, appended to the channel store's migration list in channels.rs.
pub const MIGRATION_V30: Migration = Migration::sql(30, "content-primary-metric", SCHEMA_V30);

/// Seconds in the 24-hour rule that a compared observation must fall after (the hook-test rule).
const DAY_SECS: i64 = 86_400;

const RECORD_COLUMNS: &str = "content_id, title, media, base_caption, evergreen, cooldown_days, \
    expires_at, state, withdrawal_reason, revision, created_at, updated_at";

const VARIANT_COLUMNS: &str = "variant_id, content_id, kind, text, created_at";

const OCCURRENCE_COLUMNS: &str = "occurrence_id, content_id, platform, calendar_entry_id, \
    caption_variant_id, cta_variant_id, scheduled_at, state, published_at, created_at, updated_at";

const INSERT_RECORD: &str = "
INSERT INTO content_records (venture, channel, account, content_id, title, media, base_caption,
    evergreen, cooldown_days, expires_at, state, withdrawal_reason, revision, created_at, updated_at)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'active', NULL, 1, ?11, ?11)";

const UPDATE_RECORD: &str = "
UPDATE content_records SET title = ?5, media = ?6, base_caption = ?7, evergreen = ?8,
    cooldown_days = ?9, expires_at = ?10, revision = ?11, updated_at = ?12
WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND content_id = ?4 AND revision = ?13 AND state = 'active'";

const WITHDRAW_RECORD: &str = "
UPDATE content_records SET state = 'withdrawn', withdrawal_reason = ?5, revision = ?6, updated_at = ?7
WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND content_id = ?4 AND revision = ?8 AND state = 'active'";

const BUMP_RECORD: &str = "
UPDATE content_records SET revision = ?5, updated_at = ?6
WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND content_id = ?4 AND revision = ?7 AND state = 'active'";

const CANCEL_PLANNED_FOR_RECORD: &str = "
UPDATE content_occurrences SET state = 'cancelled', updated_at = ?5
WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND content_id = ?4 AND state = 'planned'";

const INSERT_VARIANT: &str = "
INSERT INTO content_variants (venture, channel, account, variant_id, content_id, kind, text, created_at)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";

const INSERT_OCCURRENCE: &str = "
INSERT INTO content_occurrences (venture, channel, account, occurrence_id, content_id, platform,
    calendar_entry_id, caption_variant_id, cta_variant_id, scheduled_at, state, published_at, created_at, updated_at)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'planned', NULL, ?11, ?11)";

const CANCEL_OCCURRENCE: &str = "
UPDATE content_occurrences SET state = 'cancelled', updated_at = ?5
WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND occurrence_id = ?4 AND state = 'planned'";

const PUBLISH_OCCURRENCE: &str = "
UPDATE content_occurrences SET state = 'published', published_at = ?5, updated_at = ?6
WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND occurrence_id = ?4 AND state = 'planned'";

fn bad(msg: impl Into<String>) -> Fail {
    ("bad_input", msg.into())
}

fn db_err(e: impl std::fmt::Display) -> Fail {
    ("db_error", e.to_string())
}

fn internal(e: impl std::fmt::Display) -> Fail {
    ("internal", e.to_string())
}

fn encoded(value: serde_json::Result<Value>) -> Result<Value, Fail> {
    value.map_err(internal)
}

fn conflict(msg: impl Into<String>) -> Fail {
    ("revision_conflict", msg.into())
}

fn invalid_transition(msg: impl Into<String>) -> Fail {
    ("invalid_transition", msg.into())
}

fn missing(kind: &str, id: &str) -> Fail {
    ("not_found", format!("no {kind} {id} for this scope"))
}

fn open_store(root: &Path) -> Result<Connection, Fail> {
    open_channels(root)
}

/// `foreign_scope` unless the scope exactly matches a stored channel, as the other stores check.
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

fn non_empty(value: &str, name: &str) -> Result<(), Fail> {
    if value.trim().is_empty() {
        Err(bad(format!("{name} must not be empty")))
    } else {
        Ok(())
    }
}

fn one_of(value: &str, allowed: &[&str], name: &str) -> Result<(), Fail> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(bad(format!(
            "{name} must be one of {}, got {value}",
            allowed.join(", ")
        )))
    }
}

/// Trimmed text, or None when absent or blank.
fn trimmed_opt(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The optional instant as stored: parsed and rewritten in the fixed-width UTC form.
fn instant_opt(raw: Option<&str>, what: &str) -> Result<Option<String>, Fail> {
    raw.map(|s| time::parse_instant(s, what).map(time::format_instant))
        .transpose()
}

/// The clock for this command: `now` when given, else the current time.
fn clock(now: Option<&str>) -> Result<(i64, String), Fail> {
    let secs = time::now_secs(now)?;
    Ok((secs, time::format_instant(secs)))
}

/// A record id is a plain token: no `@`, which separates the parts of an occurrence id.
fn content_id_of(raw: &str) -> Result<String, Fail> {
    let id = raw.trim().to_string();
    non_empty(&id, "content_id")?;
    if id.contains('@') {
        return Err(bad("content_id must not contain @"));
    }
    Ok(id)
}

/// `true` when `at` is strictly after the expiry instant. A record without expiry never expires.
fn past_expiry(expires_at: Option<&str>, at: i64) -> Result<bool, Fail> {
    match expires_at {
        None => Ok(false),
        Some(e) => Ok(at > time::parse_instant(e, "expires_at")?),
    }
}

/// The derived state: `withdrawn` when stored so, else `expired` when the clock is past expiry, else `active`.
fn effective_state(stored: &str, expires_at: Option<&str>, now: i64) -> Result<String, Fail> {
    if stored == "withdrawn" {
        return Ok("withdrawn".to_string());
    }
    if past_expiry(expires_at, now)? {
        return Ok("expired".to_string());
    }
    Ok("active".to_string())
}

fn check_expected(cur: &ContentRecord, expected: i64) -> Result<(), Fail> {
    if cur.revision != expected {
        return Err(conflict(format!(
            "content {} is at revision {}; expected_revision {expected} is stale",
            cur.content_id, cur.revision
        )));
    }
    Ok(())
}

/// A stored record with its media still as JSON text, parsed by [`finish_record`].
struct Stored {
    record: ContentRecord,
    media_json: String,
}

fn read_stored(scope: &Scope, r: &Row<'_>) -> rightkit_db::rusqlite::Result<Stored> {
    let media_json: String = r.get(2)?;
    Ok(Stored {
        record: ContentRecord {
            scope: scope.clone(),
            content_id: r.get(0)?,
            title: r.get(1)?,
            media: Vec::new(),
            base_caption: r.get(3)?,
            evergreen: r.get::<_, i64>(4)? != 0,
            cooldown_days: r.get(5)?,
            expires_at: r.get(6)?,
            state: r.get(7)?,
            withdrawal_reason: r.get(8)?,
            revision: r.get(9)?,
            created_at: r.get(10)?,
            updated_at: r.get(11)?,
        },
        media_json,
    })
}

/// Parses the media and sets the derived state at `now`.
fn finish_record(mut stored: Stored, now: i64) -> Result<ContentRecord, Fail> {
    let media: Vec<ContentMediaItem> = serde_json::from_str(&stored.media_json).map_err(|e| {
        internal(format!(
            "stored media for {} is not valid: {e}",
            stored.record.content_id
        ))
    })?;
    stored.record.media = media;
    stored.record.state = effective_state(
        &stored.record.state,
        stored.record.expires_at.as_deref(),
        now,
    )?;
    Ok(stored.record)
}

fn load_record(conn: &Connection, scope: &Scope, content_id: &str) -> Result<Option<Stored>, Fail> {
    let sql = format!(
        "SELECT {RECORD_COLUMNS} FROM content_records
         WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND content_id = ?4"
    );
    conn.query_row(
        &sql,
        params![scope.venture, scope.channel, scope.account, content_id],
        |r| read_stored(scope, r),
    )
    .optional()
    .map_err(db_err)
}

/// The record at `content_id` with its derived state, or `not_found`.
fn record_or_missing(
    conn: &Connection,
    scope: &Scope,
    content_id: &str,
    now: i64,
) -> Result<ContentRecord, Fail> {
    let stored = load_record(conn, scope, content_id)?
        .ok_or_else(|| missing("content record", content_id))?;
    finish_record(stored, now)
}

fn read_variant(scope: &Scope, r: &Row<'_>) -> rightkit_db::rusqlite::Result<ContentVariant> {
    Ok(ContentVariant {
        scope: scope.clone(),
        variant_id: r.get(0)?,
        content_id: r.get(1)?,
        kind: r.get(2)?,
        text: r.get(3)?,
        created_at: r.get(4)?,
    })
}

fn load_variant(
    conn: &Connection,
    scope: &Scope,
    variant_id: &str,
) -> Result<Option<ContentVariant>, Fail> {
    let sql = format!(
        "SELECT {VARIANT_COLUMNS} FROM content_variants
         WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND variant_id = ?4"
    );
    conn.query_row(
        &sql,
        params![scope.venture, scope.channel, scope.account, variant_id],
        |r| read_variant(scope, r),
    )
    .optional()
    .map_err(db_err)
}

/// Variants of one record, caption kind first, each kind in creation order.
fn variants_of(
    conn: &Connection,
    scope: &Scope,
    content_id: &str,
) -> Result<Vec<ContentVariant>, Fail> {
    let sql = format!(
        "SELECT {VARIANT_COLUMNS} FROM content_variants
         WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND content_id = ?4
         ORDER BY kind ASC, created_at ASC, variant_id ASC"
    );
    let mut stmt = conn.prepare(&sql).map_err(db_err)?;
    let rows = stmt
        .query_map(
            params![scope.venture, scope.channel, scope.account, content_id],
            |r| read_variant(scope, r),
        )
        .map_err(db_err)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(db_err)?);
    }
    Ok(out)
}

fn read_occurrence(scope: &Scope, r: &Row<'_>) -> rightkit_db::rusqlite::Result<ContentOccurrence> {
    Ok(ContentOccurrence {
        scope: scope.clone(),
        occurrence_id: r.get(0)?,
        content_id: r.get(1)?,
        platform: r.get(2)?,
        calendar_entry_id: r.get(3)?,
        caption_variant_id: r.get(4)?,
        cta_variant_id: r.get(5)?,
        scheduled_at: r.get(6)?,
        state: r.get(7)?,
        published_at: r.get(8)?,
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
    })
}

fn load_occurrence(
    conn: &Connection,
    scope: &Scope,
    occurrence_id: &str,
) -> Result<Option<ContentOccurrence>, Fail> {
    let sql = format!(
        "SELECT {OCCURRENCE_COLUMNS} FROM content_occurrences
         WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND occurrence_id = ?4"
    );
    conn.query_row(
        &sql,
        params![scope.venture, scope.channel, scope.account, occurrence_id],
        |r| read_occurrence(scope, r),
    )
    .optional()
    .map_err(db_err)
}

/// Every occurrence of one record, all states, ordered by scheduled instant then occurrence id.
/// Stored instants are fixed-width UTC text, so text order is time order.
fn occurrences_of(
    conn: &Connection,
    scope: &Scope,
    content_id: &str,
) -> Result<Vec<ContentOccurrence>, Fail> {
    let sql = format!(
        "SELECT {OCCURRENCE_COLUMNS} FROM content_occurrences
         WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND content_id = ?4
         ORDER BY scheduled_at ASC, occurrence_id ASC"
    );
    let mut stmt = conn.prepare(&sql).map_err(db_err)?;
    let rows = stmt
        .query_map(
            params![scope.venture, scope.channel, scope.account, content_id],
            |r| read_occurrence(scope, r),
        )
        .map_err(db_err)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(db_err)?);
    }
    Ok(out)
}

/// A stored variant id for a record, checked for kind and ownership. `None` when the request gave none.
fn check_variant(
    conn: &Connection,
    scope: &Scope,
    content_id: &str,
    variant_id: Option<&str>,
    kind: &str,
) -> Result<Option<String>, Fail> {
    match trimmed_opt(variant_id) {
        None => Ok(None),
        Some(id) => {
            let found = load_variant(conn, scope, &id)?
                .filter(|v| v.content_id == content_id && v.kind == kind)
                .ok_or_else(|| {
                    (
                        "variant_not_found",
                        format!("no {kind} variant {id} for content {content_id} in this scope"),
                    )
                })?;
            Ok(Some(found.variant_id))
        }
    }
}

/// Earliest instant at or after `from` at least `window` seconds from every instant in `taken`.
/// Each step moves to the latest instant still in the way, so the result is the first point outside
/// every open interval `(o - window, o + window)`. A zero window never blocks.
fn earliest_free(from: i64, taken: &[i64], window: i64) -> i64 {
    let mut t = from;
    loop {
        let blocking = taken
            .iter()
            .filter(|&&o| (t - o).abs() < window)
            .max()
            .copied();
        match blocking {
            None => return t,
            Some(o) => t = o + window,
        }
    }
}

/// `content.put`: create or edit one record. Editing a withdrawn record is `invalid_transition`.
pub fn content_put(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: ContentPutInput =
        serde_json::from_value(input).map_err(|e| bad(format!("content.put input: {e}")))?;
    let content_id = content_id_of(&req.content_id)?;
    non_empty(&req.title, "title")?;
    if req.media.is_empty() {
        return Err(bad("media must list at least one item"));
    }
    for m in &req.media {
        non_empty(&m.hash, "media hash")?;
        one_of(&m.kind, &CONTENT_MEDIA_KINDS, "media kind")?;
    }
    if req.cooldown_days < 0 {
        return Err(bad(format!(
            "cooldown_days must be zero or more, got {}",
            req.cooldown_days
        )));
    }
    let expires_at = instant_opt(req.expires_at.as_deref(), "expires_at")?;
    let (now, stamp) = clock(req.now.as_deref())?;
    let media_json = serde_json::to_string(&req.media).map_err(internal)?;
    let evergreen = req.evergreen as i64;

    let mut conn = open_store(root)?;
    let tx = rightkit_db::write_txn(&mut conn).map_err(db_err)?;
    require_channel(&tx, &req.scope)?;
    let scope = &req.scope;
    match load_record(&tx, scope, &content_id)? {
        None => {
            match req.expected_revision {
                None | Some(0) => {}
                Some(n) => {
                    return Err(conflict(format!(
                        "content {content_id} does not exist (revision 0); expected_revision {n} is stale"
                    )))
                }
            }
            tx.execute(
                INSERT_RECORD,
                params![
                    scope.venture,
                    scope.channel,
                    scope.account,
                    content_id,
                    req.title,
                    media_json,
                    req.base_caption,
                    evergreen,
                    req.cooldown_days,
                    expires_at,
                    stamp
                ],
            )
            .map_err(db_err)?;
        }
        Some(stored) => {
            let cur = finish_record(stored, now)?;
            let expected = req.expected_revision.ok_or_else(|| {
                (
                    "revision_required",
                    format!("content {content_id} exists: expected_revision is required"),
                )
            })?;
            check_expected(&cur, expected)?;
            if cur.state == "withdrawn" {
                return Err(invalid_transition(format!(
                    "content {content_id} is withdrawn and cannot be edited"
                )));
            }
            let n = tx
                .execute(
                    UPDATE_RECORD,
                    params![
                        scope.venture,
                        scope.channel,
                        scope.account,
                        content_id,
                        req.title,
                        media_json,
                        req.base_caption,
                        evergreen,
                        req.cooldown_days,
                        expires_at,
                        cur.revision + 1,
                        stamp,
                        expected
                    ],
                )
                .map_err(db_err)?;
            if n != 1 {
                return Err(conflict(format!(
                    "content {content_id} changed while it was written"
                )));
            }
        }
    }
    let saved = record_or_missing(&tx, scope, &content_id, now)?;
    tx.commit().map_err(db_err)?;
    encoded(serde_json::to_value(saved))
}

/// `content.get`: one record with its variants, at the clock given by `now`.
pub fn content_get(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: ContentGetInput =
        serde_json::from_value(input).map_err(|e| bad(format!("content.get input: {e}")))?;
    let now = time::now_secs(req.now.as_deref())?;
    let conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    let record = record_or_missing(&conn, &req.scope, &req.content_id, now)?;
    let variants = variants_of(&conn, &req.scope, &req.content_id)?;
    encoded(serde_json::to_value(ContentGetOutput { record, variants }))
}

/// `content.list`: the records in one scope, filtered by derived state when given. Ordered by content id.
pub fn content_list(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: ContentListInput =
        serde_json::from_value(input).map_err(|e| bad(format!("content.list input: {e}")))?;
    if let Some(state) = req.state.as_deref() {
        one_of(state, &CONTENT_STATES, "state")?;
    }
    let now = time::now_secs(req.now.as_deref())?;
    let conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    let sql = format!(
        "SELECT {RECORD_COLUMNS} FROM content_records
         WHERE venture = ?1 AND channel = ?2 AND account = ?3
         ORDER BY content_id ASC"
    );
    let mut stmt = conn.prepare(&sql).map_err(db_err)?;
    let rows = stmt
        .query_map(
            params![req.scope.venture, req.scope.channel, req.scope.account],
            |r| read_stored(&req.scope, r),
        )
        .map_err(db_err)?;
    let mut records = Vec::new();
    for row in rows {
        let record = finish_record(row.map_err(db_err)?, now)?;
        if req.state.as_deref().is_none_or(|s| s == record.state) {
            records.push(record);
        }
    }
    encoded(serde_json::to_value(ContentListOutput {
        scope: req.scope,
        records,
    }))
}

/// `content.withdraw`: ends a record. Its planned occurrences become cancelled in the same transaction.
pub fn content_withdraw(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: ContentWithdrawInput =
        serde_json::from_value(input).map_err(|e| bad(format!("content.withdraw input: {e}")))?;
    let content_id = content_id_of(&req.content_id)?;
    let reason = req.reason.trim().to_string();
    if reason.is_empty() {
        return Err(bad("reason must not be empty"));
    }
    let (now, stamp) = clock(req.now.as_deref())?;
    let mut conn = open_store(root)?;
    let tx = rightkit_db::write_txn(&mut conn).map_err(db_err)?;
    require_channel(&tx, &req.scope)?;
    let scope = &req.scope;
    let cur = record_or_missing(&tx, scope, &content_id, now)?;
    check_expected(&cur, req.expected_revision)?;
    if cur.state == "withdrawn" {
        return Err(invalid_transition(format!(
            "content {content_id} is already withdrawn"
        )));
    }
    let planned: Vec<String> = occurrences_of(&tx, scope, &content_id)?
        .into_iter()
        .filter(|o| o.state == "planned")
        .map(|o| o.occurrence_id)
        .collect();
    let n = tx
        .execute(
            WITHDRAW_RECORD,
            params![
                scope.venture,
                scope.channel,
                scope.account,
                content_id,
                reason,
                cur.revision + 1,
                stamp,
                req.expected_revision
            ],
        )
        .map_err(db_err)?;
    if n != 1 {
        return Err(conflict(format!(
            "content {content_id} changed while it was written"
        )));
    }
    tx.execute(
        CANCEL_PLANNED_FOR_RECORD,
        params![
            scope.venture,
            scope.channel,
            scope.account,
            content_id,
            stamp
        ],
    )
    .map_err(db_err)?;
    let saved = record_or_missing(&tx, scope, &content_id, now)?;
    tx.commit().map_err(db_err)?;
    encoded(serde_json::to_value(ContentWithdrawOutput {
        record: saved,
        cancelled_occurrences: planned,
    }))
}

/// `content.add_variant`: stores one caption or CTA variant and bumps the record revision.
pub fn content_add_variant(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: ContentAddVariantInput = serde_json::from_value(input)
        .map_err(|e| bad(format!("content.add_variant input: {e}")))?;
    let content_id = content_id_of(&req.content_id)?;
    non_empty(&req.variant_id, "variant_id")?;
    non_empty(&req.text, "text")?;
    one_of(&req.kind, &CONTENT_VARIANT_KINDS, "kind")?;
    let (now, stamp) = clock(req.now.as_deref())?;
    let mut conn = open_store(root)?;
    let tx = rightkit_db::write_txn(&mut conn).map_err(db_err)?;
    require_channel(&tx, &req.scope)?;
    let scope = &req.scope;
    let cur = record_or_missing(&tx, scope, &content_id, now)?;
    check_expected(&cur, req.expected_revision)?;
    if cur.state == "withdrawn" {
        return Err(invalid_transition(format!(
            "content {content_id} is withdrawn; variants cannot be added"
        )));
    }
    if load_variant(&tx, scope, &req.variant_id)?.is_some() {
        return Err((
            "variant_exists",
            format!("variant {} already exists in this scope", req.variant_id),
        ));
    }
    tx.execute(
        INSERT_VARIANT,
        params![
            scope.venture,
            scope.channel,
            scope.account,
            req.variant_id,
            content_id,
            req.kind,
            req.text,
            stamp
        ],
    )
    .map_err(db_err)?;
    let n = tx
        .execute(
            BUMP_RECORD,
            params![
                scope.venture,
                scope.channel,
                scope.account,
                content_id,
                cur.revision + 1,
                stamp,
                req.expected_revision
            ],
        )
        .map_err(db_err)?;
    if n != 1 {
        return Err(conflict(format!(
            "content {content_id} changed while it was written"
        )));
    }
    let variant = load_variant(&tx, scope, &req.variant_id)?
        .ok_or_else(|| missing("variant", &req.variant_id))?;
    let record = record_or_missing(&tx, scope, &content_id, now)?;
    tx.commit().map_err(db_err)?;
    encoded(serde_json::to_value(ContentAddVariantOutput {
        record,
        variant,
    }))
}

/// `content.plan_occurrence`. Checks run in this order and the first failure writes nothing:
/// `content_withdrawn`; `content_expired` (clock or scheduled instant past expiry); `variant_not_found`;
/// an existing key returns that occurrence (`created` false), or `invalid_transition` when it was cancelled,
/// or `occurrence_mismatch` when its variants or calendar entry differ; `cooldown_active`.
pub fn content_plan_occurrence(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: ContentPlanOccurrenceInput = serde_json::from_value(input)
        .map_err(|e| bad(format!("content.plan_occurrence input: {e}")))?;
    let content_id = content_id_of(&req.content_id)?;
    one_of(&req.platform, &PLATFORMS, "platform")?;
    let mut at = time::parse_instant(&req.scheduled_at, "scheduled_at")?;
    let (now, stamp) = clock(req.now.as_deref())?;
    let calendar_entry_id = trimmed_opt(req.calendar_entry_id.as_deref());
    let reschedule = req.reschedule_on_cooldown.unwrap_or(false);
    let mut conn = open_store(root)?;
    let tx = rightkit_db::write_txn(&mut conn).map_err(db_err)?;
    require_channel(&tx, &req.scope)?;
    let scope = &req.scope;
    let record = record_or_missing(&tx, scope, &content_id, now)?;
    if record.state == "withdrawn" {
        return Err((
            "content_withdrawn",
            format!(
                "content {content_id} is withdrawn: {}",
                record.withdrawal_reason.clone().unwrap_or_default()
            ),
        ));
    }
    if past_expiry(record.expires_at.as_deref(), now)?
        || past_expiry(record.expires_at.as_deref(), at)?
    {
        return Err((
            "content_expired",
            format!(
                "content {content_id} expired at {}; it cannot be planned at {} or later",
                record.expires_at.clone().unwrap_or_default(),
                req.scheduled_at
            ),
        ));
    }
    let caption_variant_id = check_variant(
        &tx,
        scope,
        &content_id,
        req.caption_variant_id.as_deref(),
        "caption",
    )?;
    let cta_variant_id = check_variant(
        &tx,
        scope,
        &content_id,
        req.cta_variant_id.as_deref(),
        "cta",
    )?;

    // A rescheduled plan is replayed by its calendar entry: the same entry returns the occurrence it already got.
    if let (true, Some(entry)) = (reschedule, calendar_entry_id.as_deref()) {
        let replay = occurrences_of(&tx, scope, &content_id)?
            .into_iter()
            .find(|o| {
                o.state != "cancelled"
                    && o.platform == req.platform
                    && o.calendar_entry_id.as_deref() == Some(entry)
            });
        if let Some(stored) = replay {
            return encoded(serde_json::to_value(ContentPlanOccurrenceOutput {
                occurrence: stored,
                created: false,
            }));
        }
    }

    let mut scheduled = time::format_instant(at);
    let mut occurrence_id = format!("{content_id}@{}@{scheduled}", req.platform);
    if let Some(existing) = load_occurrence(&tx, scope, &occurrence_id)? {
        if existing.state == "cancelled" {
            return Err(invalid_transition(format!(
                "occurrence {occurrence_id} was cancelled; plan a new time instead"
            )));
        }
        if existing.caption_variant_id != caption_variant_id
            || existing.cta_variant_id != cta_variant_id
            || existing.calendar_entry_id != calendar_entry_id
        {
            return Err((
                "occurrence_mismatch",
                format!(
                    "occurrence {occurrence_id} is already planned with different variants or calendar entry"
                ),
            ));
        }
        return encoded(serde_json::to_value(ContentPlanOccurrenceOutput {
            occurrence: existing,
            created: false,
        }));
    }

    let live: Vec<ContentOccurrence> = occurrences_of(&tx, scope, &content_id)?
        .into_iter()
        .filter(|o| o.state != "cancelled" && o.platform == req.platform)
        .collect();
    // A record that is not evergreen runs once per platform: a second live occurrence is refused.
    if !record.evergreen {
        if let Some(prior) = live.first() {
            return Err((
                "content_not_evergreen",
                format!(
                    "content {content_id} is not evergreen and already has occurrence {} on {}; only an evergreen record is reused",
                    prior.occurrence_id, req.platform
                ),
            ));
        }
    }
    let mut taken = Vec::with_capacity(live.len());
    for o in &live {
        taken.push(time::parse_instant(&o.scheduled_at, "scheduled_at")?);
    }
    let window = record.cooldown_days * SECS_PER_DAY;
    if let Some(idx) = taken.iter().position(|&o| (at - o).abs() < window) {
        let blocker = &live[idx];
        let earliest = earliest_free(at, &taken, window);
        if !reschedule {
            return Err((
                "cooldown_active",
                format!(
                    "content {content_id} on {} is cooling down: occurrence {} at {} is within {} days; earliest_allowed_at {}",
                    req.platform,
                    blocker.occurrence_id,
                    blocker.scheduled_at,
                    record.cooldown_days,
                    time::format_instant(earliest)
                ),
            ));
        }
        // Reschedule: move to the earliest allowed instant, then check expiry and the key at that instant.
        at = earliest;
        if past_expiry(record.expires_at.as_deref(), at)? {
            return Err((
                "content_expired",
                format!(
                    "content {content_id} expires at {}; the earliest allowed time {} is after it",
                    record.expires_at.clone().unwrap_or_default(),
                    time::format_instant(at)
                ),
            ));
        }
        scheduled = time::format_instant(at);
        occurrence_id = format!("{content_id}@{}@{scheduled}", req.platform);
        if load_occurrence(&tx, scope, &occurrence_id)?.is_some() {
            return Err(invalid_transition(format!(
                "occurrence {occurrence_id} exists at the earliest allowed time; plan again with a later time"
            )));
        }
    }

    tx.execute(
        INSERT_OCCURRENCE,
        params![
            scope.venture,
            scope.channel,
            scope.account,
            occurrence_id,
            content_id,
            req.platform,
            calendar_entry_id,
            caption_variant_id,
            cta_variant_id,
            scheduled,
            stamp
        ],
    )
    .map_err(db_err)?;
    let saved = load_occurrence(&tx, scope, &occurrence_id)?
        .ok_or_else(|| missing("occurrence", &occurrence_id))?;
    tx.commit().map_err(db_err)?;
    encoded(serde_json::to_value(ContentPlanOccurrenceOutput {
        occurrence: saved,
        created: true,
    }))
}

/// `content.cancel_occurrence`: only a planned occurrence can be cancelled.
pub fn content_cancel_occurrence(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: ContentCancelOccurrenceInput = serde_json::from_value(input)
        .map_err(|e| bad(format!("content.cancel_occurrence input: {e}")))?;
    non_empty(&req.occurrence_id, "occurrence_id")?;
    let (_, stamp) = clock(req.now.as_deref())?;
    let mut conn = open_store(root)?;
    let tx = rightkit_db::write_txn(&mut conn).map_err(db_err)?;
    require_channel(&tx, &req.scope)?;
    let scope = &req.scope;
    let cur = load_occurrence(&tx, scope, &req.occurrence_id)?
        .ok_or_else(|| missing("occurrence", &req.occurrence_id))?;
    if cur.state != "planned" {
        return Err(invalid_transition(format!(
            "occurrence {} is {}: only a planned occurrence can be cancelled",
            req.occurrence_id, cur.state
        )));
    }
    let n = tx
        .execute(
            CANCEL_OCCURRENCE,
            params![
                scope.venture,
                scope.channel,
                scope.account,
                req.occurrence_id,
                stamp
            ],
        )
        .map_err(db_err)?;
    if n != 1 {
        return Err(conflict(format!(
            "occurrence {} changed while it was written",
            req.occurrence_id
        )));
    }
    let saved = load_occurrence(&tx, scope, &req.occurrence_id)?
        .ok_or_else(|| missing("occurrence", &req.occurrence_id))?;
    tx.commit().map_err(db_err)?;
    encoded(serde_json::to_value(saved))
}

/// `content.mark_published`: only a planned occurrence can be marked published.
pub fn content_mark_published(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: ContentMarkPublishedInput = serde_json::from_value(input)
        .map_err(|e| bad(format!("content.mark_published input: {e}")))?;
    non_empty(&req.occurrence_id, "occurrence_id")?;
    let (_, stamp) = clock(req.now.as_deref())?;
    let published_at =
        instant_opt(req.published_at.as_deref(), "published_at")?.unwrap_or_else(|| stamp.clone());
    let mut conn = open_store(root)?;
    let tx = rightkit_db::write_txn(&mut conn).map_err(db_err)?;
    require_channel(&tx, &req.scope)?;
    let scope = &req.scope;
    let cur = load_occurrence(&tx, scope, &req.occurrence_id)?
        .ok_or_else(|| missing("occurrence", &req.occurrence_id))?;
    if cur.state != "planned" {
        return Err(invalid_transition(format!(
            "occurrence {} is {}: only a planned occurrence can be marked published",
            req.occurrence_id, cur.state
        )));
    }
    let n = tx
        .execute(
            PUBLISH_OCCURRENCE,
            params![
                scope.venture,
                scope.channel,
                scope.account,
                req.occurrence_id,
                published_at,
                stamp
            ],
        )
        .map_err(db_err)?;
    if n != 1 {
        return Err(conflict(format!(
            "occurrence {} changed while it was written",
            req.occurrence_id
        )));
    }
    let saved = load_occurrence(&tx, scope, &req.occurrence_id)?
        .ok_or_else(|| missing("occurrence", &req.occurrence_id))?;
    tx.commit().map_err(db_err)?;
    encoded(serde_json::to_value(saved))
}

/// `content.next_allowed`: the earliest instant at or after `now` the record may run on the platform.
/// Null with a reason when withdrawn or when that instant is past expiry.
pub fn content_next_allowed(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: ContentNextAllowedInput = serde_json::from_value(input)
        .map_err(|e| bad(format!("content.next_allowed input: {e}")))?;
    let content_id = content_id_of(&req.content_id)?;
    one_of(&req.platform, &PLATFORMS, "platform")?;
    let (now, stamp) = clock(req.now.as_deref())?;
    let conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    let record = record_or_missing(&conn, &req.scope, &content_id, now)?;
    let (earliest_allowed_at, reason) = if record.state == "withdrawn" {
        (None, Some("content_withdrawn".to_string()))
    } else {
        let mut taken = Vec::new();
        for o in occurrences_of(&conn, &req.scope, &content_id)? {
            if o.state != "cancelled" && o.platform == req.platform {
                taken.push(time::parse_instant(&o.scheduled_at, "scheduled_at")?);
            }
        }
        let t = earliest_free(now, &taken, record.cooldown_days * SECS_PER_DAY);
        if past_expiry(record.expires_at.as_deref(), t)? {
            (None, Some("content_expired".to_string()))
        } else {
            (Some(time::format_instant(t)), None)
        }
    };
    encoded(serde_json::to_value(ContentNextAllowedOutput {
        scope: req.scope,
        content_id,
        platform: req.platform,
        now: stamp,
        state: record.state,
        earliest_allowed_at,
        reason,
    }))
}

/// Counts for each stored variant of one kind. Only published occurrences count toward the minimum.
/// Occurrences with no variant of that kind are not counted.
fn variant_counts(
    variants: &[ContentVariant],
    kind: &str,
    occurrences: &[ContentOccurrence],
    min: i64,
    measured: bool,
    values: &BTreeMap<String, i128>,
) -> Vec<VariantCount> {
    variants
        .iter()
        .filter(|v| v.kind == kind)
        .map(|v| {
            let mine: Vec<&ContentOccurrence> = occurrences
                .iter()
                .filter(|o| picked(o, kind) == Some(v.variant_id.as_str()))
                .collect();
            let published = mine.iter().filter(|o| o.state == "published").count() as i64;
            let compared_values: Vec<i128> = mine
                .iter()
                .filter(|o| o.state == "published")
                .filter_map(|o| values.get(&o.occurrence_id).copied())
                .collect();
            let compared = compared_values.len() as i64;
            let total: i128 = compared_values.iter().sum();
            VariantCount {
                variant_id: v.variant_id.clone(),
                text: v.text.clone(),
                planned: mine.iter().filter(|o| o.state == "planned").count() as i64,
                published,
                cancelled: mine.iter().filter(|o| o.state == "cancelled").count() as i64,
                compared,
                total: total.to_string(),
                meets_minimum: if measured {
                    compared >= min
                } else {
                    published >= min
                },
            }
        })
        .collect()
}

/// The variant id an occurrence holds for one kind.
fn picked<'a>(o: &'a ContentOccurrence, kind: &str) -> Option<&'a str> {
    if kind == "caption" {
        o.caption_variant_id.as_deref()
    } else {
        o.cta_variant_id.as_deref()
    }
}

/// Reasons one kind of variants is not comparable. Empty when it is.
fn kind_reasons(kind: &str, counts: &[VariantCount], min: i64, measured: bool) -> Vec<String> {
    let mut reasons = Vec::new();
    if counts.len() < 2 {
        reasons.push(format!(
            "fewer than two {kind} variants are stored ({})",
            counts.len()
        ));
    }
    for c in counts {
        if !c.meets_minimum {
            reasons.push(if measured {
                format!(
                    "{kind} variant {} has {} compared occurrences; the minimum is {min}",
                    c.variant_id, c.compared
                )
            } else {
                format!(
                    "{kind} variant {} has {} published occurrences; the minimum is {min}",
                    c.variant_id, c.published
                )
            });
        }
    }
    reasons
}

/// The strict maximum mean of one compared kind, by exact integer cross-multiplication. `Ok(None)` when the kind
/// has no variant. `Err` names the tie. Callers check the counts and the minimum first.
fn kind_winner(kind: &str, counts: &[VariantCount]) -> Result<Option<String>, String> {
    let mut top: Vec<&VariantCount> = Vec::new();
    for c in counts {
        let c_total: i128 = c.total.parse().unwrap_or(0);
        let leader = top
            .first()
            .map(|t| (t.total.parse::<i128>().unwrap_or(0), t.compared as i128));
        match leader {
            None => top.push(c),
            Some((t_total, t_compared)) => {
                let lhs = c_total * t_compared;
                let rhs = t_total * c.compared as i128;
                if lhs > rhs {
                    top.clear();
                    top.push(c);
                } else if lhs == rhs {
                    top.push(c);
                }
            }
        }
    }
    match top.as_slice() {
        [] => Ok(None),
        [only] => Ok(Some(only.variant_id.clone())),
        many => Err(format!(
            "{kind} variants {} tie on the primary metric mean; no winner is named",
            many.iter()
                .map(|c| c.variant_id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The primary metric declared for a record, with its declaration instant.
fn declared(
    conn: &Connection,
    scope: &Scope,
    content_id: &str,
) -> Result<Option<(String, String)>, Fail> {
    conn.query_row(
        "SELECT metric, declared_at FROM content_primary_metrics
         WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND content_id = ?4",
        params![scope.venture, scope.channel, scope.account, content_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .map_err(db_err)
}

/// The compared value of one published entry, by the hook-test rule: the lifetime api observation of `metric`
/// closest to 24 hours after the entry's latest published job, never before 24 hours and never after `now`.
/// An unavailable or non-integer value is not compared (None).
fn hook_value(
    conn: &Connection,
    scope: &Scope,
    entry_id: &str,
    metric: &str,
    now: i64,
) -> Result<Option<i128>, Fail> {
    let job = conn
        .query_row(
            "SELECT job_id, updated_at FROM publish_jobs
             WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND entry_id = ?4 AND status = 'complete'
             ORDER BY updated_at DESC, job_id DESC LIMIT 1",
            params![scope.venture, scope.channel, scope.account, entry_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(db_err)?;
    let Some((job_id, published_at)) = job else {
        return Ok(None);
    };
    let published = time::parse_instant(&published_at, "published_at")?;
    let mut stmt = conn
        .prepare(
            "SELECT state, value, observed_at FROM metric_observations
             WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND job_id = ?4 AND metric = ?5
               AND window_name = 'lifetime'",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map(
            params![scope.venture, scope.channel, scope.account, job_id, metric],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .map_err(db_err)?;
    let mut best: Option<(i64, String, Option<String>)> = None;
    for row in rows {
        let (state, value, observed_at) = row.map_err(db_err)?;
        let at = time::parse_instant(&observed_at, "observed_at")?;
        let age = at - published;
        if age < DAY_SECS || at > now {
            continue;
        }
        if best.as_ref().is_none_or(|(b, _, _)| age < *b) {
            best = Some((age, state, value));
        }
    }
    Ok(match best {
        Some((_, state, Some(text))) if state == "value" => text.parse::<i128>().ok(),
        _ => None,
    })
}

/// The compared value of every published occurrence that has a calendar entry, keyed by occurrence id.
fn occurrence_values(
    conn: &Connection,
    scope: &Scope,
    occurrences: &[ContentOccurrence],
    metric: &str,
    now: i64,
) -> Result<BTreeMap<String, i128>, Fail> {
    let mut out = BTreeMap::new();
    for o in occurrences {
        if o.state != "published" {
            continue;
        }
        let Some(entry) = o.calendar_entry_id.as_deref() else {
            continue;
        };
        if let Some(value) = hook_value(conn, scope, entry, metric, now)? {
            out.insert(o.occurrence_id.clone(), value);
        }
    }
    Ok(out)
}

/// `content.declare_metric`: names the one primary metric that may decide a record's experiment. Declared once:
/// the same metric again returns the stored declaration (`created` false), a different one is
/// `metric_already_declared`. Only the lifetime window is stored, as the hook test does.
pub fn content_declare_metric(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: ContentDeclareMetricInput = serde_json::from_value(input)
        .map_err(|e| bad(format!("content.declare_metric input: {e}")))?;
    let content_id = content_id_of(&req.content_id)?;
    let metric = req.metric.trim().to_string();
    if metric.is_empty()
        || metric.len() > 64
        || !metric
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.')
    {
        return Err(bad(
            "metric must be lowercase letters, digits, underscores or dots, at most 64 characters",
        ));
    }
    let (now, stamp) = clock(req.now.as_deref())?;
    let mut conn = open_store(root)?;
    let tx = rightkit_db::write_txn(&mut conn).map_err(db_err)?;
    require_channel(&tx, &req.scope)?;
    record_or_missing(&tx, &req.scope, &content_id, now)?;
    if let Some((existing, declared_at)) = declared(&tx, &req.scope, &content_id)? {
        if existing != metric {
            return Err((
                "metric_already_declared",
                format!("content {content_id} already declares primary metric {existing}"),
            ));
        }
        return encoded(serde_json::to_value(ContentDeclareMetricOutput {
            scope: req.scope,
            content_id,
            metric,
            window: "lifetime".to_string(),
            declared_at,
            created: false,
        }));
    }
    tx.execute(
        "INSERT INTO content_primary_metrics (venture, channel, account, content_id, metric, window_name, declared_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'lifetime', ?6)",
        params![
            req.scope.venture,
            req.scope.channel,
            req.scope.account,
            content_id,
            metric,
            stamp
        ],
    )
    .map_err(db_err)?;
    tx.commit().map_err(db_err)?;
    encoded(serde_json::to_value(ContentDeclareMetricOutput {
        scope: req.scope,
        content_id,
        metric,
        window: "lifetime".to_string(),
        declared_at: stamp,
        created: true,
    }))
}

/// `content.experiment_summary`: counts per variant. With no declared primary metric it names no winner. With a
/// declared metric (content.declare_metric) it names the strict maximum mean of each compared kind, once every
/// variant of that kind has at least `min_occurrences` compared occurrences; a tie or a shortfall is
/// `insufficient_evidence` with the reason.
pub fn content_experiment_summary(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: ContentExperimentSummaryInput = serde_json::from_value(input)
        .map_err(|e| bad(format!("content.experiment_summary input: {e}")))?;
    let content_id = content_id_of(&req.content_id)?;
    let min = req.min_occurrences.unwrap_or(DEFAULT_MIN_OCCURRENCES);
    if min < 1 {
        return Err(bad(format!(
            "min_occurrences must be at least 1, got {min}"
        )));
    }
    let (now, _) = clock(req.now.as_deref())?;
    let conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    if load_record(&conn, &req.scope, &content_id)?.is_none() {
        return Err(missing("content record", &content_id));
    }
    let primary = declared(&conn, &req.scope, &content_id)?.map(|(metric, _)| metric);
    let variants = variants_of(&conn, &req.scope, &content_id)?;
    let occurrences = occurrences_of(&conn, &req.scope, &content_id)?;
    let values = match primary.as_deref() {
        Some(metric) => occurrence_values(&conn, &req.scope, &occurrences, metric, now)?,
        None => BTreeMap::new(),
    };
    let measured = primary.is_some();
    let caption = variant_counts(&variants, "caption", &occurrences, min, measured, &values);
    let cta = variant_counts(&variants, "cta", &occurrences, min, measured, &values);

    let mut reasons = kind_reasons("caption", &caption, min, measured);
    reasons.extend(kind_reasons("cta", &cta, min, measured));
    let mut caption_winner = None;
    let mut cta_winner = None;
    if measured && reasons.is_empty() {
        match kind_winner("caption", &caption) {
            Ok(winner) => caption_winner = winner,
            Err(message) => reasons.push(message),
        }
        match kind_winner("cta", &cta) {
            Ok(winner) => cta_winner = winner,
            Err(message) => reasons.push(message),
        }
    }
    let (verdict, reason) = match (&primary, reasons.is_empty()) {
        (None, true) => (
            "evidence_sufficient",
            "every compared variant meets the minimum; no primary metric is declared, so no winner is named".to_string(),
        ),
        (None, false) => ("insufficient_evidence", reasons.join("; ")),
        (Some(metric), true) => (
            "winner",
            format!("primary metric {metric} decides: the strict maximum mean in each compared kind"),
        ),
        (Some(_), false) => ("insufficient_evidence", reasons.join("; ")),
    };
    encoded(serde_json::to_value(ContentExperimentSummaryOutput {
        scope: req.scope,
        content_id,
        min_occurrences: min,
        verdict: verdict.to_string(),
        reason,
        primary_metric: primary,
        caption_winner,
        cta_winner,
        caption,
        cta,
    }))
}

/// Routes one content method. Unknown methods are `unknown_method`.
pub fn dispatch(root: &Path, method: &str, input: Value) -> Result<Value, Fail> {
    match method {
        "content.put" => content_put(root, input),
        "content.get" => content_get(root, input),
        "content.list" => content_list(root, input),
        "content.withdraw" => content_withdraw(root, input),
        "content.add_variant" => content_add_variant(root, input),
        "content.plan_occurrence" => content_plan_occurrence(root, input),
        "content.cancel_occurrence" => content_cancel_occurrence(root, input),
        "content.mark_published" => content_mark_published(root, input),
        "content.next_allowed" => content_next_allowed(root, input),
        "content.experiment_summary" => content_experiment_summary(root, input),
        "content.declare_metric" => content_declare_metric(root, input),
        other => Err(("unknown_method", format!("no content method {other}"))),
    }
}
