//! Research digests: dated findings the caller supplies (this module fetches nothing), turned into draft
//! calendar entries. Schema v16 of the channel store (one migration list, in channels.rs). Its two tables are
//! `research_digests` and `digest_applications`. This module makes NO network call and reads no clock: `now`
//! is an input, and it is used only for stamps.
//!
//! - `digest.put` stores a digest once. The same id with the same content is a no-op; different content is
//!   `digest_mismatch`. An entry without a source is `bad_input` naming the entry.
//! - `digest.plan` lists what `digest.apply` would add: one draft per entry not yet applied. Entries take the
//!   free slots in order. A slot is free when its UTC instant is not already the instant of a calendar entry
//!   in scope (an entry this plan would create does not count). Entries beyond the free slots are `unplaced`.
//!   Every slot is checked (zone, wall time, day inside the week) before anything is written.
//! - `digest.apply` does, per planned item: `brief.put` (lineage in the payload), then `calendar.put` (state
//!   draft, caption starting with the placeholder prefix so `calendar.approve` refuses it), then records the
//!   application. Ids are derived from digest id and entry id, so a retry reuses the brief and the entry and
//!   records nothing twice. The brief and entry writes go through calendar.rs; this module writes no calendar
//!   table.
//! - `digest.applied` lists which digest entries produced which calendar entries.
//!
//! - Slot reservation (schema v34): `digest.apply` also reserves the UTC instant each applied entry took, in
//!   `digest_slot_reservations`, one row per instant in scope. A plan treats a reserved instant as taken even after
//!   its draft moved to another time, so two digests never plan onto the same slot. `digest.release` frees one
//!   reservation (the calendar entry is untouched); `digest.reserved` lists them.
//!
//! Every row is keyed by venture, channel and account, and every command checks its scope against a stored
//! channel (`foreign_scope` otherwise).
use std::collections::BTreeSet;
use std::path::Path;

use crate::digest_protocol::{
    DigestApplication, DigestAppliedInput, DigestAppliedOutput, DigestApplyInput,
    DigestApplyOutput, DigestEntry, DigestEntryInput, DigestGetInput, DigestListInput,
    DigestListOutput, DigestPlanInput, DigestPlanItem, DigestPlanOutput, DigestPutInput,
    DigestPutOutput, DigestRecord, DigestReleaseInput, DigestReleaseOutput, DigestReservedInput,
    DigestReservedOutput, DigestSlot, DigestSlotReservation, Scope,
};
use rightkit_db::rusqlite::{params, Connection, OptionalExtension};
use rightkit_db::Migration;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::host::{host, Fail};
use rightkit_social_publish::calendar_time as time;

/// Methods that write. A dry run reports these and writes nothing.
pub const WRITE_METHODS: &[&str] = &["digest.put", "digest.apply", "digest.release"];

/// Every method this module routes.
pub const METHODS: &[&str] = &[
    "digest.put",
    "digest.get",
    "digest.list",
    "digest.plan",
    "digest.apply",
    "digest.applied",
    "digest.release",
    "digest.reserved",
];

const SCHEMA_V16: &str = "
CREATE TABLE research_digests (
    venture TEXT NOT NULL,
    channel TEXT NOT NULL,
    account TEXT NOT NULL,
    digest_id TEXT NOT NULL,
    digest_date TEXT NOT NULL,
    entries_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (venture, channel, account, digest_id),
    FOREIGN KEY (venture, channel, account) REFERENCES channels (venture, channel, account)
);
CREATE TABLE digest_applications (
    venture TEXT NOT NULL,
    channel TEXT NOT NULL,
    account TEXT NOT NULL,
    digest_id TEXT NOT NULL,
    digest_entry_id TEXT NOT NULL,
    brief_id TEXT NOT NULL,
    entry_id TEXT NOT NULL,
    applied_at TEXT NOT NULL,
    PRIMARY KEY (venture, channel, account, digest_id, digest_entry_id),
    UNIQUE (venture, channel, account, entry_id),
    FOREIGN KEY (venture, channel, account, digest_id) REFERENCES research_digests (venture, channel, account, digest_id),
    FOREIGN KEY (venture, channel, account, entry_id) REFERENCES calendar_entries (venture, channel, account, entry_id)
);
";

/// Schema v16, appended to the channel store's migration list in channels.rs after v14.
pub const MIGRATION_V16: Migration = Migration::sql(16, "research-digest", SCHEMA_V16);

const SCHEMA_V34: &str = "
CREATE TABLE digest_slot_reservations (
    venture TEXT NOT NULL,
    channel TEXT NOT NULL,
    account TEXT NOT NULL,
    scheduled_at TEXT NOT NULL,
    digest_id TEXT NOT NULL,
    digest_entry_id TEXT NOT NULL,
    entry_id TEXT NOT NULL,
    reserved_at TEXT NOT NULL,
    PRIMARY KEY (venture, channel, account, scheduled_at),
    UNIQUE (venture, channel, account, digest_id, digest_entry_id),
    FOREIGN KEY (venture, channel, account) REFERENCES channels (venture, channel, account)
);
";

/// Schema v34, appended to the channel store's migration list in channels.rs after v33. Applications made before
/// this migration hold no reservation: their drafts still take their instants as calendar entries.
pub const MIGRATION_V34: Migration = Migration::sql(34, "digest-slot-reservations", SCHEMA_V34);

/// `OR IGNORE` skips an instant another apply reserved first; the plan already excluded reserved instants, so this
/// only guards a concurrent apply.
const INSERT_RESERVATION: &str = "
INSERT OR IGNORE INTO digest_slot_reservations (venture, channel, account, scheduled_at, digest_id,
    digest_entry_id, entry_id, reserved_at)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";

/// Ids are 1 to this many bytes: letters, digits, `-` and `_`. The derived ids join them with `.`.
const MAX_ID: usize = 48;
/// The seven days a week plan covers, in seconds.
const WEEK_SECS: i64 = 7 * 86_400;
/// The brief type every digest brief carries.
const BRIEF_TYPE: &str = "research_digest";

const INSERT_DIGEST: &str = "
INSERT INTO research_digests (venture, channel, account, digest_id, digest_date, entries_json, created_at)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";

/// `OR IGNORE` only skips an application that a concurrent apply already recorded. The row count says whether
/// this call recorded it, so the diff lists only what this call added.
const INSERT_APPLICATION: &str = "
INSERT OR IGNORE INTO digest_applications (venture, channel, account, digest_id, digest_entry_id, brief_id, entry_id,
    applied_at)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";

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

fn not_found(digest_id: &str) -> Fail {
    (
        "not_found",
        format!("no research digest {digest_id} for this scope"),
    )
}

fn parse<T: DeserializeOwned>(input: Value, method: &str) -> Result<T, Fail> {
    serde_json::from_value(input).map_err(|e| bad(format!("{method} input: {e}")))
}

fn open_store(root: &Path) -> Result<Connection, Fail> {
    host()?.open_store(root)
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

fn id_ok(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= MAX_ID
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn check_id(raw: &str, what: &str) -> Result<(), Fail> {
    if id_ok(raw) {
        Ok(())
    } else {
        Err(bad(format!(
            "{what} must be 1 to {MAX_ID} letters, digits, '-' or '_', got {raw:?}"
        )))
    }
}

/// A `YYYY-MM-DD` date, checked as the start of its day.
fn check_date(raw: &str, what: &str) -> Result<(), Fail> {
    // rightkit-time 0.1.0 has no date-only parser; a midnight RFC 3339 instant validates the civil date.
    let ok = raw.len() == 10
        && rightkit_time::DateTime::parse_rfc3339(&format!("{raw}T00:00:00Z")).is_some();
    if ok {
        Ok(())
    } else {
        Err(bad(format!(
            "{what} must be a date YYYY-MM-DD, got {raw:?}"
        )))
    }
}

/// The first day after the week that starts on `week_start` (`YYYY-MM-DD`).
fn week_end(week_start: &str) -> Result<String, Fail> {
    let start = time::parse_instant(&format!("{week_start}T00:00:00Z"), "week_start")?;
    Ok(time::format_instant(start + WEEK_SECS)[..10].to_string())
}

/// A source is an http or https url, or `path:line` with a line of 1 or more.
fn source_ok(raw: &str) -> bool {
    if let Some(rest) = raw
        .strip_prefix("https://")
        .or_else(|| raw.strip_prefix("http://"))
    {
        return !rest.is_empty();
    }
    match raw.rsplit_once(':') {
        Some((path, line)) => !path.trim().is_empty() && line.parse::<u64>().is_ok_and(|n| n >= 1),
        None => false,
    }
}

fn entry_id_for(digest_id: &str, entry_id: &str) -> String {
    format!("digest_entry.{digest_id}.{entry_id}")
}

fn brief_id_for(digest_id: &str, entry_id: &str) -> String {
    format!("digest_brief.{digest_id}.{entry_id}")
}

/// Checks every entry and returns the stored form. A digest has at least one entry.
fn check_entries(input: &[DigestEntryInput]) -> Result<Vec<DigestEntry>, Fail> {
    if input.is_empty() {
        return Err(bad("entries must not be empty"));
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::with_capacity(input.len());
    for e in input {
        check_id(&e.entry_id, "entry_id")?;
        if !seen.insert(e.entry_id.as_str()) {
            return Err(bad(format!(
                "entry {} appears more than once in the digest",
                e.entry_id
            )));
        }
        let source = e.source.as_deref().map(str::trim).unwrap_or("");
        if source.is_empty() {
            return Err(bad(format!(
                "entry {} has no source: give an http(s) url or path:line",
                e.entry_id
            )));
        }
        if !source_ok(source) {
            return Err(bad(format!(
                "entry {} source must be an http(s) url or path:line, got {source}",
                e.entry_id
            )));
        }
        let fetched_on = e.fetched_on.trim();
        check_date(fetched_on, "fetched_on").map_err(|_| {
            bad(format!(
                "entry {} fetched_on must be a date YYYY-MM-DD, got {fetched_on:?}",
                e.entry_id
            ))
        })?;
        for (value, what) in [
            (&e.topic, "topic"),
            (&e.finding, "finding"),
            (&e.platform, "platform"),
            (&e.format, "format"),
        ] {
            if value.trim().is_empty() {
                return Err(bad(format!("entry {} has an empty {what}", e.entry_id)));
            }
        }
        out.push(DigestEntry {
            entry_id: e.entry_id.clone(),
            topic: e.topic.trim().to_string(),
            finding: e.finding.trim().to_string(),
            source: source.to_string(),
            fetched_on: fetched_on.to_string(),
            platform: e.platform.trim().to_string(),
            format: e.format.trim().to_string(),
        });
    }
    Ok(out)
}

fn record(
    scope: &Scope,
    digest_id: &str,
    digest_date: String,
    entries_json: &str,
    created_at: String,
) -> Result<DigestRecord, Fail> {
    Ok(DigestRecord {
        scope: scope.clone(),
        digest_id: digest_id.to_string(),
        digest_date,
        entries: serde_json::from_str(entries_json).map_err(internal)?,
        created_at,
    })
}

fn load_digest(
    conn: &Connection,
    scope: &Scope,
    digest_id: &str,
) -> Result<Option<DigestRecord>, Fail> {
    let row: Option<(String, String, String)> = conn
        .query_row(
            "SELECT digest_date, entries_json, created_at FROM research_digests
             WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND digest_id = ?4",
            params![scope.venture, scope.channel, scope.account, digest_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(db_err)?;
    row.map(|(digest_date, entries_json, created_at)| {
        record(scope, digest_id, digest_date, &entries_json, created_at)
    })
    .transpose()
}

/// Every application of one digest, ordered by apply time then digest entry id.
fn applications_of(
    conn: &Connection,
    scope: &Scope,
    digest_id: &str,
) -> Result<Vec<DigestApplication>, Fail> {
    let mut stmt = conn
        .prepare(
            "SELECT digest_entry_id, brief_id, entry_id, applied_at FROM digest_applications
             WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND digest_id = ?4
             ORDER BY applied_at ASC, digest_entry_id ASC",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map(
            params![scope.venture, scope.channel, scope.account, digest_id],
            |r| {
                Ok(DigestApplication {
                    digest_id: digest_id.to_string(),
                    digest_entry_id: r.get(0)?,
                    brief_id: r.get(1)?,
                    entry_id: r.get(2)?,
                    applied_at: r.get(3)?,
                })
            },
        )
        .map_err(db_err)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(db_err)?);
    }
    Ok(out)
}

/// The result of planning: the items to add, the entries with no free slot, and the applications already made.
struct Planned {
    items: Vec<DigestPlanItem>,
    unplaced: Vec<String>,
    already_applied: Vec<DigestApplication>,
}

/// The pure part of the plan, shared by `digest.plan` and `digest.apply`. Reads the store, writes nothing.
fn build_plan(
    conn: &Connection,
    scope: &Scope,
    digest: &DigestRecord,
    week_start: &str,
    slots: &[DigestSlot],
) -> Result<Planned, Fail> {
    check_date(week_start, "week_start")?;
    let week_end = week_end(week_start)?;

    // Every slot is resolved before any write: its zone and wall time, and its day inside the week.
    let mut resolved = Vec::with_capacity(slots.len());
    for (index, slot) in slots.iter().enumerate() {
        let fold = time::parse_fold(slot.fold.as_deref())?;
        let secs = time::to_utc(&slot.timezone, &slot.local_time, fold)?;
        let day = slot.local_time.get(..10).unwrap_or("");
        if day < week_start || day >= week_end.as_str() {
            return Err(bad(format!(
                "slot {index} local_time {} is outside the week starting {week_start}",
                slot.local_time
            )));
        }
        resolved.push((index, time::format_instant(secs)));
    }

    let already_applied = applications_of(conn, scope, &digest.digest_id)?;
    let pending: Vec<DigestEntry> = {
        let applied_ids: BTreeSet<&str> = already_applied
            .iter()
            .map(|a| a.digest_entry_id.as_str())
            .collect();
        digest
            .entries
            .iter()
            .filter(|e| !applied_ids.contains(e.entry_id.as_str()))
            .cloned()
            .collect()
    };
    let planned_ids: BTreeSet<String> = pending
        .iter()
        .map(|e| entry_id_for(&digest.digest_id, &e.entry_id))
        .collect();

    // Instants already used by calendar entries in scope, except the rows this plan would itself write.
    let mut stmt = conn
        .prepare(
            "SELECT entry_id, scheduled_at FROM calendar_entries
             WHERE venture = ?1 AND channel = ?2 AND account = ?3",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map(params![scope.venture, scope.channel, scope.account], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(db_err)?;
    let mut taken = BTreeSet::new();
    for row in rows {
        let (entry_id, at) = row.map_err(db_err)?;
        if !planned_ids.contains(&entry_id) {
            taken.insert(at);
        }
    }

    // Reserved instants stay taken after the draft that took them moved to another time.
    let mut reserved_stmt = conn
        .prepare(
            "SELECT scheduled_at FROM digest_slot_reservations
             WHERE venture = ?1 AND channel = ?2 AND account = ?3",
        )
        .map_err(db_err)?;
    let reserved_rows = reserved_stmt
        .query_map(params![scope.venture, scope.channel, scope.account], |r| {
            r.get::<_, String>(0)
        })
        .map_err(db_err)?;
    for row in reserved_rows {
        taken.insert(row.map_err(db_err)?);
    }

    let mut used = BTreeSet::new();
    let mut free = Vec::new();
    for (index, at) in resolved {
        if taken.contains(&at) || !used.insert(at.clone()) {
            continue;
        }
        free.push((index, at));
    }

    let mut free_iter = free.into_iter();
    let mut items = Vec::new();
    let mut unplaced = Vec::new();
    for entry in pending {
        match free_iter.next() {
            Some((index, at)) => {
                let slot = &slots[index];
                items.push(DigestPlanItem {
                    digest_entry_id: entry.entry_id.clone(),
                    brief_id: brief_id_for(&digest.digest_id, &entry.entry_id),
                    entry_id: entry_id_for(&digest.digest_id, &entry.entry_id),
                    slot_index: index as i64,
                    scheduled_at: at,
                    timezone: slot.timezone.clone(),
                    local_time: slot.local_time.clone(),
                    fold: slot.fold.clone(),
                    entry,
                });
            }
            None => unplaced.push(entry.entry_id),
        }
    }
    Ok(Planned {
        items,
        unplaced,
        already_applied,
    })
}

/// Route one method of this module. Unknown methods are `unknown_method`.
pub fn dispatch(root: &Path, method: &str, input: Value) -> Result<Value, Fail> {
    match method {
        "digest.put" => put(root, input),
        "digest.get" => get(root, input),
        "digest.list" => list(root, input),
        "digest.plan" => plan_command(root, input),
        "digest.apply" => apply(root, input),
        "digest.applied" => applied(root, input),
        "digest.release" => release(root, input),
        "digest.reserved" => reserved(root, input),
        other => Err(("unknown_method", format!("unknown method: {other}"))),
    }
}

/// `digest.put`: stores the digest once. The same content again is `unchanged`; different content is
/// `digest_mismatch`, and nothing is written.
fn put(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: DigestPutInput = parse(input, "digest.put")?;
    check_id(&req.digest_id, "digest_id")?;
    check_date(&req.digest_date, "digest_date")?;
    let now = time::parse_instant(&req.now, "now")?;
    let entries = check_entries(&req.entries)?;
    let entries_json = serde_json::to_string(&entries).map_err(internal)?;
    let entry_count = entries.len() as i64;

    let mut conn = open_store(root)?;
    let tx = rightkit_db::write_txn(&mut conn).map_err(db_err)?;
    require_channel(&tx, &req.scope)?;
    let unchanged = match load_digest(&tx, &req.scope, &req.digest_id)? {
        Some(old) => {
            if old.digest_date != req.digest_date || old.entries != entries {
                return Err((
                    "digest_mismatch",
                    format!(
                        "digest {} is already stored with different content; a digest is immutable",
                        req.digest_id
                    ),
                ));
            }
            true
        }
        None => {
            tx.execute(
                INSERT_DIGEST,
                params![
                    req.scope.venture,
                    req.scope.channel,
                    req.scope.account,
                    req.digest_id,
                    req.digest_date,
                    entries_json,
                    time::format_instant(now)
                ],
            )
            .map_err(db_err)?;
            false
        }
    };
    tx.commit().map_err(db_err)?;
    encoded(serde_json::to_value(DigestPutOutput {
        scope: req.scope,
        digest_id: req.digest_id,
        digest_date: req.digest_date,
        entry_count,
        unchanged,
    }))
}

/// `digest.get`: one digest in scope.
fn get(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: DigestGetInput = parse(input, "digest.get")?;
    let conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    let digest =
        load_digest(&conn, &req.scope, &req.digest_id)?.ok_or_else(|| not_found(&req.digest_id))?;
    encoded(serde_json::to_value(digest))
}

/// `digest.list`: every digest in scope, ordered by digest date then id.
fn list(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: DigestListInput = parse(input, "digest.list")?;
    let conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    let mut stmt = conn
        .prepare(
            "SELECT digest_id, digest_date, entries_json, created_at FROM research_digests
             WHERE venture = ?1 AND channel = ?2 AND account = ?3
             ORDER BY digest_date ASC, digest_id ASC",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map(
            params![req.scope.venture, req.scope.channel, req.scope.account],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .map_err(db_err)?;
    let mut digests = Vec::new();
    for row in rows {
        let (digest_id, digest_date, entries_json, created_at) = row.map_err(db_err)?;
        digests.push(record(
            &req.scope,
            &digest_id,
            digest_date,
            &entries_json,
            created_at,
        )?);
    }
    encoded(serde_json::to_value(DigestListOutput {
        scope: req.scope,
        digests,
    }))
}

/// `digest.plan`: what `digest.apply` would add. Writes nothing.
fn plan_command(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: DigestPlanInput = parse(input, "digest.plan")?;
    let conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    let digest =
        load_digest(&conn, &req.scope, &req.digest_id)?.ok_or_else(|| not_found(&req.digest_id))?;
    let planned = build_plan(&conn, &req.scope, &digest, &req.week_start, &req.slots)?;
    encoded(serde_json::to_value(DigestPlanOutput {
        scope: req.scope,
        digest_id: req.digest_id,
        week_start: req.week_start,
        items: planned.items,
        unplaced: planned.unplaced,
        already_applied: planned.already_applied,
    }))
}

/// `digest.apply`: creates the brief and the draft calendar entry for each planned item, in plan order, and
/// records each application. A replay reuses what exists and returns an empty `diff`.
fn apply(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: DigestApplyInput = parse(input, "digest.apply")?;
    let now = time::parse_instant(&req.now, "now")?;
    let stamp = time::format_instant(now);

    let (digest, planned) = {
        let conn = open_store(root)?;
        require_channel(&conn, &req.scope)?;
        let digest = load_digest(&conn, &req.scope, &req.digest_id)?
            .ok_or_else(|| not_found(&req.digest_id))?;
        let planned = build_plan(&conn, &req.scope, &digest, &req.week_start, &req.slots)?;
        (digest, planned)
    };

    let mut diff = Vec::new();
    for item in planned.items {
        let brief = host()?.brief_put(
            root,
            json!({
                "scope": req.scope,
                "brief_id": item.brief_id,
                "brief_type": BRIEF_TYPE,
                "title": format!(
                    "Research digest {} entry {}: {}",
                    req.digest_id, item.digest_entry_id, item.entry.topic
                ),
                "payload": {
                    "digest_id": req.digest_id,
                    "digest_date": digest.digest_date,
                    "digest_entry_id": item.digest_entry_id,
                    "topic": item.entry.topic,
                    "finding": item.entry.finding,
                    "source": item.entry.source,
                    "fetched_on": item.entry.fetched_on,
                    "platform": item.entry.platform,
                    "format": item.entry.format,
                    "week_start": req.week_start,
                },
                "now": req.now,
            }),
        )?;
        let brief_revision = brief
            .get("revision")
            .and_then(Value::as_i64)
            .ok_or_else(|| internal("brief.put returned no revision"))?;

        match host()?.calendar_get(
            root,
            json!({ "scope": req.scope, "entry_id": item.entry_id, "now": req.now }),
        ) {
            Ok(_) => {}
            Err(e) if e.0 != "not_found" => return Err(e),
            Err(_) => {
                host()?.calendar_put(
                    root,
                    json!({
                        "scope": req.scope,
                        "entry_id": item.entry_id,
                        "brief_id": item.brief_id,
                        "brief_revision": brief_revision,
                        "platform": item.entry.platform,
                        "format": item.entry.format,
                        "caption": format!(
                            "{} Research digest {}, entry {} ({}): {}. Source: {}",
                            host()?.placeholder_caption_prefix(),
                            req.digest_id,
                            item.digest_entry_id,
                            item.entry.topic,
                            item.entry.finding,
                            item.entry.source
                        ),
                        "media": [],
                        "timezone": item.timezone,
                        "local_time": item.local_time,
                        "fold": item.fold,
                        "now": req.now,
                    }),
                )?;
            }
        }

        let mut conn = open_store(root)?;
        let tx = rightkit_db::write_txn(&mut conn).map_err(db_err)?;
        require_channel(&tx, &req.scope)?;
        let added = tx
            .execute(
                INSERT_APPLICATION,
                params![
                    req.scope.venture,
                    req.scope.channel,
                    req.scope.account,
                    req.digest_id,
                    item.digest_entry_id,
                    item.brief_id,
                    item.entry_id,
                    stamp
                ],
            )
            .map_err(db_err)?;
        if added == 1 {
            tx.execute(
                INSERT_RESERVATION,
                params![
                    req.scope.venture,
                    req.scope.channel,
                    req.scope.account,
                    item.scheduled_at,
                    req.digest_id,
                    item.digest_entry_id,
                    item.entry_id,
                    stamp
                ],
            )
            .map_err(db_err)?;
        }
        tx.commit().map_err(db_err)?;
        if added == 1 {
            diff.push(DigestApplication {
                digest_id: req.digest_id.clone(),
                digest_entry_id: item.digest_entry_id,
                brief_id: item.brief_id,
                entry_id: item.entry_id,
                applied_at: stamp.clone(),
            });
        }
    }

    encoded(serde_json::to_value(DigestApplyOutput {
        scope: req.scope,
        digest_id: req.digest_id,
        week_start: req.week_start,
        diff,
        already_applied: planned.already_applied,
        unplaced: planned.unplaced,
    }))
}

/// `digest.applied`: which digest entries produced which calendar entries.
fn applied(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: DigestAppliedInput = parse(input, "digest.applied")?;
    let conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    if load_digest(&conn, &req.scope, &req.digest_id)?.is_none() {
        return Err(not_found(&req.digest_id));
    }
    let applications = applications_of(&conn, &req.scope, &req.digest_id)?;
    encoded(serde_json::to_value(DigestAppliedOutput {
        scope: req.scope,
        digest_id: req.digest_id,
        applications,
    }))
}

/// `digest.release`: frees the slot one digest entry reserved. The calendar entry is untouched. An entry with no
/// reservation reports `released: false`; a digest that is not stored is `not_found`.
fn release(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: DigestReleaseInput = parse(input, "digest.release")?;
    check_id(&req.digest_id, "digest_id")?;
    check_id(&req.digest_entry_id, "digest_entry_id")?;
    let mut conn = open_store(root)?;
    let tx = rightkit_db::write_txn(&mut conn).map_err(db_err)?;
    require_channel(&tx, &req.scope)?;
    if load_digest(&tx, &req.scope, &req.digest_id)?.is_none() {
        return Err(not_found(&req.digest_id));
    }
    let n = tx
        .execute(
            "DELETE FROM digest_slot_reservations
             WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND digest_id = ?4 AND digest_entry_id = ?5",
            params![
                req.scope.venture,
                req.scope.channel,
                req.scope.account,
                req.digest_id,
                req.digest_entry_id
            ],
        )
        .map_err(db_err)?;
    tx.commit().map_err(db_err)?;
    encoded(serde_json::to_value(DigestReleaseOutput {
        scope: req.scope,
        digest_id: req.digest_id,
        digest_entry_id: req.digest_entry_id,
        released: n == 1,
    }))
}

/// `digest.reserved`: the reserved slots in scope (one digest when `digest_id` is given), by instant.
fn reserved(root: &Path, input: Value) -> Result<Value, Fail> {
    let req: DigestReservedInput = parse(input, "digest.reserved")?;
    let conn = open_store(root)?;
    require_channel(&conn, &req.scope)?;
    let mut stmt = conn
        .prepare(
            "SELECT digest_id, digest_entry_id, entry_id, scheduled_at, reserved_at FROM digest_slot_reservations
             WHERE venture = ?1 AND channel = ?2 AND account = ?3 AND (?4 IS NULL OR digest_id = ?4)
             ORDER BY scheduled_at ASC, digest_entry_id ASC",
        )
        .map_err(db_err)?;
    let rows = stmt
        .query_map(
            params![
                req.scope.venture,
                req.scope.channel,
                req.scope.account,
                req.digest_id
            ],
            |r| {
                Ok(DigestSlotReservation {
                    digest_id: r.get(0)?,
                    digest_entry_id: r.get(1)?,
                    entry_id: r.get(2)?,
                    scheduled_at: r.get(3)?,
                    reserved_at: r.get(4)?,
                })
            },
        )
        .map_err(db_err)?;
    let mut reservations = Vec::new();
    for row in rows {
        reservations.push(row.map_err(db_err)?);
    }
    encoded(serde_json::to_value(DigestReservedOutput {
        scope: req.scope,
        reservations,
    }))
}
