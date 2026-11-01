//! The brief and calendar commands over a host-implemented [`CalendarStore`]: `brief.put`, `brief.get`,
//! `calendar.put`, `get`, `list`, `approve`, `reject`, `request_approval`, `pause`, `resume`,
//! `resolve_failure` and `engine.clock`. A brief is versioned content a calendar entry points at; an entry
//! has a revision, an approval state and a state of its own. Every command checks its scope against a stored
//! channel (`foreign_scope` otherwise). Nothing here publishes anything.
use rightkit_social_publish::calendar_time::{self as time, Fold};
use rightkit_social_publish::util::{
    bad, db_err, in_txn, invalid_transition, non_empty, parse_input, require_channel, to_value,
    WriteTxn,
};
use serde_json::Value;

use crate::protocol::{
    BriefGetInput, BriefPutInput, BriefPutOutput, BriefRecord, CalendarApproveInput, CalendarEntry,
    CalendarGetInput, CalendarListInput, CalendarListOutput, CalendarPutInput, CalendarRejectInput,
    CalendarRequestApprovalInput, CalendarStateInput, DstNote, EngineClock, Scope, SpacingConflict,
    PUT_STATES,
};
use crate::Fail;

/// Minutes before `scheduled_at` that a draft or media_pending entry must be ready by.
pub const DEFAULT_PREP_MINUTES: i64 = 30;

/// Minimum minutes between two unfinished entries of one account and platform before `conflict` is reported.
/// It matches the publish pacing default (`min_gap_secs` 900).
pub const DEFAULT_MIN_SPACING_MINUTES: i64 = 15;
/// Largest `min_spacing_minutes` a read accepts (one day).
const MAX_MIN_SPACING_MINUTES: i64 = 1440;
/// Bounds on the first comment and the settings labels.
const MAX_FIRST_COMMENT_BYTES: usize = 8192;
const MAX_SETTINGS: usize = 16;
const MAX_SETTING_BYTES: usize = 128;

/// Methods that write. A dry run reports these and writes nothing.
pub const WRITE_METHODS: [&str; 8] = [
    "brief.put",
    "calendar.resolve_failure",
    "calendar.put",
    "calendar.approve",
    "calendar.reject",
    "calendar.request_approval",
    "calendar.pause",
    "calendar.resume",
];

/// Schema v40: operator resolutions of failed entries (`calendar.resolve_failure`). A resolution hides the failure from

/// States `calendar.pause` accepts.
const PAUSABLE: [&str; 4] = ["draft", "media_pending", "ready", "scheduled"];

/// Captions written by the engine itself start with this; `calendar.approve` refuses them.
pub const PLACEHOLDER_CAPTION_PREFIX: &str = "PLACEHOLDER, replace before approval.";

fn not_found(entry_id: &str) -> Fail {
    (
        "not_found",
        format!("no calendar entry {entry_id} for this scope"),
    )
}

fn conflict(msg: impl Into<String>) -> Fail {
    ("revision_conflict", msg.into())
}

/// The minimum spacing a read applies: the default, or the caller's whole minutes from 0 (off) to one day.
fn spacing_minutes(raw: Option<i64>) -> Result<i64, Fail> {
    match raw {
        None => Ok(DEFAULT_MIN_SPACING_MINUTES),
        Some(n) if (0..=MAX_MIN_SPACING_MINUTES).contains(&n) => Ok(n),
        Some(n) => Err(bad(format!(
            "min_spacing_minutes must be 0 to {MAX_MIN_SPACING_MINUTES}, got {n}"
        ))),
    }
}

fn prep_minutes(raw: Option<i64>) -> Result<i64, Fail> {
    match raw {
        None => Ok(DEFAULT_PREP_MINUTES),
        Some(n) if n >= 0 => Ok(n),
        Some(n) => Err(bad(format!("prep_minutes must be zero or more, got {n}"))),
    }
}

/// Column list shared by every entry read. Index 9 is `media_json` and index 23 is `settings_json`, both parsed

/// Fill `effective_state`: `late` for a draft or media_pending entry whose preparation deadline
/// (`scheduled_at` minus `prep` minutes) is before `now`. The stored state is unchanged.
fn with_effective(mut entry: CalendarEntry, now: i64, prep: i64) -> Result<CalendarEntry, Fail> {
    let scheduled = time::parse_instant(&entry.scheduled_at, "scheduled_at")?;
    let pending = matches!(entry.state.as_str(), "draft" | "media_pending");
    entry.effective_state = if pending && now > scheduled - prep * 60 {
        "late".to_string()
    } else {
        entry.state.clone()
    };
    Ok(entry)
}

/// The daylight-saving note of an entry. A wall time that occurs twice (clocks go back) fires once, at the
/// occurrence the entry was stored with; every other entry has nothing to report. A zone or wall time the table
/// cannot resolve reports nothing here (it was refused when the entry was put).
fn dst_note(entry: &CalendarEntry) -> DstNote {
    let ambiguous = matches!(
        time::to_utc(&entry.timezone, &entry.local_time, None),
        Err(("time_ambiguous", _))
    );
    if !ambiguous {
        return DstNote::default();
    }
    let earlier = time::to_utc(&entry.timezone, &entry.local_time, Some(Fold::Earlier))
        .ok()
        .map(time::format_instant);
    let which = if earlier.as_deref() == Some(entry.scheduled_at.as_str()) {
        "earlier"
    } else {
        "later"
    };
    DstNote {
        kind: "ambiguous_fires_once".to_string(),
        note: Some(format!(
            "{} occurs twice in {} (the clocks go back); this entry fires once, at the {which} occurrence, {}",
            entry.local_time, entry.timezone, entry.scheduled_at
        )),
    }
}

/// States of an entry that still intends to publish: only these can conflict on spacing.
const UNFINISHED: [&str; 4] = ["draft", "media_pending", "ready", "scheduled"];

/// One stored brief revision.
#[derive(Debug, Clone)]
pub struct BriefRow {
    pub revision: i64,
    pub brief_type: String,
    pub title: String,
    /// The payload as JSON text.
    pub payload_json: String,
    pub created_at: String,
}

/// An approval-state write (approve, reject, request approval). Applies only where the stored revision
/// equals `expected_revision`; the revision itself does not change.
#[derive(Debug, Clone)]
pub struct ApprovalWrite<'a> {
    pub scope: &'a Scope,
    pub entry_id: &'a str,
    pub expected_revision: i64,
    /// `approved`, `rejected` or `pending`.
    pub approval_state: &'a str,
    pub approved_entry_revision: Option<i64>,
    pub approved_by: Option<&'a str>,
    pub policy_revision: Option<i64>,
    pub rejection_reason: Option<&'a str>,
    pub updated_at: &'a str,
}

/// A state write (pause, resume). Applies only where the stored revision equals `expected_revision`.
#[derive(Debug, Clone)]
pub struct StateWrite<'a> {
    pub scope: &'a Scope,
    pub entry_id: &'a str,
    pub expected_revision: i64,
    pub state: &'a str,
    pub paused_from: Option<&'a str>,
    pub new_revision: i64,
    pub updated_at: &'a str,
}

/// An operator's resolution of a failed entry.
#[derive(Debug, Clone)]
pub struct FailureResolution<'a> {
    pub scope: &'a Scope,
    pub entry_id: &'a str,
    pub entry_revision: i64,
    pub entry_updated_at: &'a str,
    pub resolved_by: &'a str,
    pub note: &'a str,
    pub resolved_at: &'a str,
}

/// What the host app provides: the brief, calendar entry and failure-resolution tables. Every method is one
/// read or one write on the host's own database, on the caller's transaction.
pub trait CalendarStore: WriteTxn {
    /// The brief revision (the latest when `revision` is `None`), if stored.
    fn brief(
        &self,
        scope: &Scope,
        brief_id: &str,
        revision: Option<i64>,
    ) -> Result<Option<BriefRow>, Fail>;
    /// Stores one brief revision.
    fn insert_brief(&self, scope: &Scope, brief_id: &str, brief: &BriefRow) -> Result<(), Fail>;
    /// One entry in scope, as stored (`effective_state`, `dst` and `conflict` empty).
    fn load_entry(&self, scope: &Scope, entry_id: &str) -> Result<Option<CalendarEntry>, Fail>;
    /// Stores a new entry exactly as given (revision 1, approval `none`).
    fn insert_entry(&self, entry: &CalendarEntry) -> Result<(), Fail>;
    /// Overwrites the content, state, `paused_from`, revision, approval state and `updated_at` of the entry
    /// (and its first comment and settings) where the stored revision equals `expected_revision`. Returns
    /// whether a row changed.
    fn update_entry(&self, entry: &CalendarEntry, expected_revision: i64) -> Result<bool, Fail>;
    /// Writes an approval state; see [`ApprovalWrite`].
    fn write_approval(&self, write: &ApprovalWrite<'_>) -> Result<(), Fail>;
    /// Writes a state; see [`StateWrite`]. Returns whether a row changed.
    fn write_state(&self, write: &StateWrite<'_>) -> Result<bool, Fail>;
    /// Entries in scope with `scheduled_at` from `from` (inclusive) to `to` (exclusive), either absent for
    /// open, ordered by `scheduled_at` then entry id.
    fn list_entries(
        &self,
        scope: &Scope,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<Vec<CalendarEntry>, Fail>;
    /// `(entry_id, scheduled_at)` of the other entries of the account and platform that are `draft`,
    /// `media_pending`, `ready` or `scheduled` with `scheduled_at` strictly between `lower` and `upper`,
    /// ordered by `scheduled_at` then entry id.
    fn spacing_candidates(
        &self,
        entry: &CalendarEntry,
        lower: &str,
        upper: &str,
    ) -> Result<Vec<(String, String)>, Fail>;
    /// Records (or replaces) a failure resolution.
    fn put_resolution(&self, resolution: &FailureResolution<'_>) -> Result<(), Fail>;
}

/// The nearest other unfinished entry of the same account and platform whose scheduled instant is strictly
/// closer than `spacing` minutes (ties: the smaller entry id). Exactly the spacing apart is allowed. An entry
/// that is itself finished or paused, or a spacing of 0, has no conflict.
fn spacing_conflict(
    store: &dyn CalendarStore,
    entry: &CalendarEntry,
    spacing: i64,
) -> Result<Option<SpacingConflict>, Fail> {
    if spacing == 0 || !UNFINISHED.contains(&entry.state.as_str()) {
        return Ok(None);
    }
    let at = time::parse_instant(&entry.scheduled_at, "scheduled_at")?;
    let window = spacing * 60;
    let lower = time::format_instant(at - window);
    let upper = time::format_instant(at + window);
    let mut nearest: Option<(i64, String)> = None;
    for (other_id, other_at) in store.spacing_candidates(entry, &lower, &upper)? {
        let gap = (time::parse_instant(&other_at, "scheduled_at")? - at).abs();
        let closer = match &nearest {
            None => true,
            Some((best, best_id)) => gap < *best || (gap == *best && other_id < *best_id),
        };
        if closer {
            nearest = Some((gap, other_id));
        }
    }
    Ok(nearest.map(|(_, with_entry_id)| SpacingConflict {
        with_entry_id,
        minimum_spacing_minutes: spacing,
    }))
}

/// The entry as every command returns it: `effective_state`, the DST note and the spacing conflict filled.
fn present(
    store: &dyn CalendarStore,
    entry: CalendarEntry,
    now: i64,
    prep: i64,
    spacing: i64,
) -> Result<CalendarEntry, Fail> {
    let mut entry = with_effective(entry, now, prep)?;
    entry.dst = dst_note(&entry);
    entry.conflict = spacing_conflict(store, &entry, spacing)?;
    Ok(entry)
}

/// [`present`] with the default preparation and spacing.
fn present_default(
    store: &dyn CalendarStore,
    entry: CalendarEntry,
    now: i64,
) -> Result<CalendarEntry, Fail> {
    present(
        store,
        entry,
        now,
        DEFAULT_PREP_MINUTES,
        DEFAULT_MIN_SPACING_MINUTES,
    )
}

fn check_expected(entry: &CalendarEntry, expected: i64) -> Result<(), Fail> {
    if entry.revision != expected {
        return Err(conflict(format!(
            "entry {} is at revision {}; expected_revision {expected} is stale",
            entry.entry_id, entry.revision
        )));
    }
    Ok(())
}

/// The first comment, when given, is non-empty text of bounded size; each setting label is non-empty, bounded,
/// free of control characters and listed once.
fn check_comment_and_settings(
    first_comment: Option<&str>,
    settings: &[String],
) -> Result<(), Fail> {
    if let Some(text) = first_comment {
        non_empty(text, "first_comment")?;
        if text.len() > MAX_FIRST_COMMENT_BYTES {
            return Err(bad(format!(
                "first_comment must be at most {MAX_FIRST_COMMENT_BYTES} bytes, got {}",
                text.len()
            )));
        }
    }
    if settings.len() > MAX_SETTINGS {
        return Err(bad(format!(
            "settings must hold at most {MAX_SETTINGS} labels, got {}",
            settings.len()
        )));
    }
    for (i, label) in settings.iter().enumerate() {
        non_empty(label, "settings label")?;
        if label.len() > MAX_SETTING_BYTES || label.chars().any(char::is_control) {
            return Err(bad(format!(
                "a settings label must be at most {MAX_SETTING_BYTES} bytes with no control characters"
            )));
        }
        if settings[..i].contains(label) {
            return Err(bad(format!("settings label {label:?} is listed twice")));
        }
    }
    Ok(())
}

/// `brief.put`: the next revision of a brief, or the same revision when the content is identical to

/// `brief.put`: the next revision of a brief, or the same revision when the content is identical to
/// the latest one. Type, title and payload are the content compared.
pub fn brief_put(store: &dyn CalendarStore, input: Value) -> Result<Value, Fail> {
    let req: BriefPutInput = parse_input(input, "brief.put")?;
    non_empty(&req.brief_id, "brief_id")?;
    non_empty(&req.brief_type, "brief_type")?;
    non_empty(&req.title, "title")?;
    if req.payload.is_null() {
        return Err(bad("payload must not be null"));
    }
    let now = time::format_instant(time::now_secs(req.now.as_deref())?);
    let (revision, unchanged) = in_txn(store, || {
        require_channel(store, &req.scope)?;
        let latest = store.brief(&req.scope, &req.brief_id, None)?;
        let same = match &latest {
            Some(row) => {
                row.brief_type == req.brief_type
                    && row.title == req.title
                    && serde_json::from_str::<Value>(&row.payload_json).map_err(db_err)?
                        == req.payload
            }
            None => false,
        };
        match (&latest, same) {
            (Some(row), true) => Ok((row.revision, true)),
            _ => {
                let next = latest.as_ref().map_or(1, |l| l.revision + 1);
                store.insert_brief(
                    &req.scope,
                    &req.brief_id,
                    &BriefRow {
                        revision: next,
                        brief_type: req.brief_type.clone(),
                        title: req.title.clone(),
                        payload_json: req.payload.to_string(),
                        created_at: now.clone(),
                    },
                )?;
                Ok((next, false))
            }
        }
    })?;
    to_value(&BriefPutOutput {
        scope: req.scope,
        brief_id: req.brief_id,
        revision,
        unchanged,
    })
}

/// `brief.get`: one revision, or the latest when `revision` is absent.
pub fn brief_get(store: &dyn CalendarStore, input: Value) -> Result<Value, Fail> {
    let req: BriefGetInput = parse_input(input, "brief.get")?;
    require_channel(store, &req.scope)?;
    let row = store
        .brief(&req.scope, &req.brief_id, req.revision)?
        .ok_or_else(|| {
            (
                "not_found",
                format!(
                    "no brief {} revision {:?} for this scope",
                    req.brief_id, req.revision
                ),
            )
        })?;
    let payload: Value = serde_json::from_str(&row.payload_json).map_err(db_err)?;
    to_value(&BriefRecord {
        scope: req.scope,
        brief_id: req.brief_id,
        revision: row.revision,
        brief_type: row.brief_type,
        title: row.title,
        payload,
        created_at: row.created_at,
    })
}

/// `calendar.put`: create (`expected_revision` absent or 0) or update (`expected_revision` must match).
/// A byte-identical update is a no-op and keeps the revision.
pub fn calendar_put(store: &dyn CalendarStore, input: Value) -> Result<Value, Fail> {
    let req: CalendarPutInput = parse_input(input, "calendar.put")?;
    non_empty(&req.entry_id, "entry_id")?;
    non_empty(&req.brief_id, "brief_id")?;
    non_empty(&req.platform, "platform")?;
    non_empty(&req.format, "format")?;
    non_empty(&req.timezone, "timezone")?;
    non_empty(&req.local_time, "local_time")?;
    if req.brief_revision < 1 {
        return Err(bad("brief_revision must be 1 or more"));
    }
    for m in &req.media {
        non_empty(&m.hash, "media.hash")?;
        non_empty(&m.kind, "media.kind")?;
    }
    check_comment_and_settings(req.first_comment.as_deref(), &req.settings)?;
    let state = match req.state.as_deref() {
        None => "draft".to_string(),
        Some(s) if PUT_STATES.contains(&s) => s.to_string(),
        Some(s) => {
            return Err(bad(format!(
                "state {s} cannot be put; use one of {} (paused comes from calendar.pause, late is derived)",
                PUT_STATES.join(", ")
            )))
        }
    };
    let fold: Option<Fold> = time::parse_fold(req.fold.as_deref())?;
    let scheduled_at = time::format_instant(time::to_utc(&req.timezone, &req.local_time, fold)?);
    let now_secs = time::now_secs(req.now.as_deref())?;
    let stamp = time::format_instant(now_secs);

    let out = in_txn(store, || {
        require_channel(store, &req.scope)?;
        if store
            .brief(&req.scope, &req.brief_id, Some(req.brief_revision))?
            .is_none()
        {
            return Err((
                "brief_not_found",
                format!(
                    "brief {} revision {} is not stored for this scope",
                    req.brief_id, req.brief_revision
                ),
            ));
        }

        match store.load_entry(&req.scope, &req.entry_id)? {
            None => {
                match req.expected_revision {
                    None | Some(0) => {}
                    Some(n) => {
                        return Err(conflict(format!(
                            "entry {} does not exist (revision 0); expected_revision {n} is stale",
                            req.entry_id
                        )))
                    }
                }
                store.insert_entry(&CalendarEntry {
                    scope: req.scope.clone(),
                    entry_id: req.entry_id.clone(),
                    brief_id: req.brief_id.clone(),
                    brief_revision: req.brief_revision,
                    platform: req.platform.clone(),
                    format: req.format.clone(),
                    caption: req.caption.clone(),
                    media: req.media.clone(),
                    scheduled_at: scheduled_at.clone(),
                    timezone: req.timezone.clone(),
                    local_time: req.local_time.clone(),
                    state: state.clone(),
                    effective_state: String::new(),
                    paused_from: None,
                    revision: 1,
                    approval_state: "none".to_string(),
                    approved_entry_revision: None,
                    approved_by: None,
                    policy_revision: None,
                    rejection_reason: None,
                    updated_at: stamp.clone(),
                    first_comment: req.first_comment.clone(),
                    settings: req.settings.clone(),
                    dst: DstNote::default(),
                    conflict: None,
                })?;
            }
            Some(cur) => {
                let expected = req.expected_revision.ok_or_else(|| {
                    (
                        "revision_required",
                        format!(
                            "entry {} exists: expected_revision is required",
                            req.entry_id
                        ),
                    )
                })?;
                check_expected(&cur, expected)?;
                if cur.state == "published" {
                    return Err(invalid_transition(format!(
                        "entry {} is published and cannot change",
                        req.entry_id
                    )));
                }
                // A paused entry keeps its state; a put that names a state sets the state resume returns to.
                let (target, paused_from) = if cur.state == "paused" {
                    let from = if req.state.is_some() {
                        Some(state.clone())
                    } else {
                        cur.paused_from.clone()
                    };
                    ("paused".to_string(), from)
                } else {
                    (state.clone(), None)
                };
                let content_changed = cur.brief_id != req.brief_id
                    || cur.brief_revision != req.brief_revision
                    || cur.platform != req.platform
                    || cur.format != req.format
                    || cur.caption != req.caption
                    || cur.media != req.media
                    || cur.scheduled_at != scheduled_at
                    || cur.timezone != req.timezone
                    || cur.local_time != req.local_time
                    || cur.first_comment != req.first_comment
                    || cur.settings != req.settings;
                let state_changed = cur.state != target || cur.paused_from != paused_from;
                if !content_changed && !state_changed {
                    // Nothing changed: return the stored entry; the transaction ends unwritten.
                    return present_default(store, cur, now_secs);
                }
                let approval_state = if content_changed && cur.approval_state == "approved" {
                    "invalidated".to_string()
                } else {
                    cur.approval_state.clone()
                };
                let changed = store.update_entry(
                    &CalendarEntry {
                        brief_id: req.brief_id.clone(),
                        brief_revision: req.brief_revision,
                        platform: req.platform.clone(),
                        format: req.format.clone(),
                        caption: req.caption.clone(),
                        media: req.media.clone(),
                        scheduled_at: scheduled_at.clone(),
                        timezone: req.timezone.clone(),
                        local_time: req.local_time.clone(),
                        state: target,
                        paused_from,
                        revision: cur.revision + 1,
                        approval_state,
                        updated_at: stamp.clone(),
                        first_comment: req.first_comment.clone(),
                        settings: req.settings.clone(),
                        ..cur
                    },
                    expected,
                )?;
                if !changed {
                    return Err(conflict(format!(
                        "entry {} changed while it was written",
                        req.entry_id
                    )));
                }
            }
        }
        let saved = store
            .load_entry(&req.scope, &req.entry_id)?
            .ok_or_else(|| not_found(&req.entry_id))?;
        present_default(store, saved, now_secs)
    })?;
    to_value(&out)
}

/// `calendar.get`: one entry in scope, with `effective_state`.
pub fn calendar_get(store: &dyn CalendarStore, input: Value) -> Result<Value, Fail> {
    let req: CalendarGetInput = parse_input(input, "calendar.get")?;
    let now = time::now_secs(req.now.as_deref())?;
    let prep = prep_minutes(req.prep_minutes)?;
    let spacing = spacing_minutes(req.min_spacing_minutes)?;
    require_channel(store, &req.scope)?;
    let entry = store
        .load_entry(&req.scope, &req.entry_id)?
        .ok_or_else(|| not_found(&req.entry_id))?;
    to_value(&present(store, entry, now, prep, spacing)?)
}

/// `calendar.list`: entries in scope, `from` inclusive to `to` exclusive, ordered by `scheduled_at`
/// then entry id.
pub fn calendar_list(store: &dyn CalendarStore, input: Value) -> Result<Value, Fail> {
    let req: CalendarListInput = parse_input(input, "calendar.list")?;
    let now = time::now_secs(req.now.as_deref())?;
    let prep = prep_minutes(req.prep_minutes)?;
    let spacing = spacing_minutes(req.min_spacing_minutes)?;
    let from = req
        .from
        .as_deref()
        .map(|s| time::parse_instant(s, "from"))
        .transpose()?;
    let to = req
        .to
        .as_deref()
        .map(|s| time::parse_instant(s, "to"))
        .transpose()?;
    if let (Some(a), Some(b)) = (from, to) {
        if a >= b {
            return Err(bad("from must be before to"));
        }
    }
    let from = from.map(time::format_instant);
    let to = to.map(time::format_instant);
    require_channel(store, &req.scope)?;
    let mut entries = Vec::new();
    for entry in store.list_entries(&req.scope, from.as_deref(), to.as_deref())? {
        entries.push(present(store, entry, now, prep, spacing)?);
    }
    to_value(&CalendarListOutput {
        scope: req.scope,
        entries,
    })
}

/// The entry in scope at exactly `expected_revision`.
fn entry_at(
    store: &dyn CalendarStore,
    scope: &Scope,
    entry_id: &str,
    expected_revision: i64,
) -> Result<CalendarEntry, Fail> {
    let cur = store
        .load_entry(scope, entry_id)?
        .ok_or_else(|| not_found(entry_id))?;
    check_expected(&cur, expected_revision)?;
    Ok(cur)
}

/// Reloads the entry after an approval write and presents it.
fn saved_entry(
    store: &dyn CalendarStore,
    scope: &Scope,
    entry_id: &str,
    now: i64,
) -> Result<CalendarEntry, Fail> {
    let saved = store
        .load_entry(scope, entry_id)?
        .ok_or_else(|| not_found(entry_id))?;
    present_default(store, saved, now)
}

/// `calendar.approve`: approves exactly `expected_revision`. Stamps the revision and approver; the
/// revision itself does not change.
pub fn calendar_approve(store: &dyn CalendarStore, input: Value) -> Result<Value, Fail> {
    let req: CalendarApproveInput = parse_input(input, "calendar.approve")?;
    non_empty(&req.approved_by, "approved_by")?;
    let now = time::now_secs(req.now.as_deref())?;
    let stamp = time::format_instant(now);
    let out = in_txn(store, || {
        require_channel(store, &req.scope)?;
        let cur = entry_at(store, &req.scope, &req.entry_id, req.expected_revision)?;
        // A machine-written draft (a remake proposal's entry) carries a marked caption: it is never approvable.
        if cur.caption.starts_with(PLACEHOLDER_CAPTION_PREFIX) {
            return Err((
                "placeholder_caption",
                format!(
                    "entry {} still has a placeholder caption: replace it before approval",
                    req.entry_id
                ),
            ));
        }
        store.write_approval(&ApprovalWrite {
            scope: &req.scope,
            entry_id: &req.entry_id,
            expected_revision: req.expected_revision,
            approval_state: "approved",
            approved_entry_revision: Some(req.expected_revision),
            approved_by: Some(&req.approved_by),
            policy_revision: Some(req.policy_revision),
            rejection_reason: None,
            updated_at: &stamp,
        })?;
        saved_entry(store, &req.scope, &req.entry_id, now)
    })?;
    to_value(&out)
}

/// `calendar.reject`: records the reason and clears any approval. The revision does not change.
pub fn calendar_reject(store: &dyn CalendarStore, input: Value) -> Result<Value, Fail> {
    let req: CalendarRejectInput = parse_input(input, "calendar.reject")?;
    non_empty(&req.reason, "reason")?;
    let now = time::now_secs(req.now.as_deref())?;
    let stamp = time::format_instant(now);
    let out = in_txn(store, || {
        require_channel(store, &req.scope)?;
        entry_at(store, &req.scope, &req.entry_id, req.expected_revision)?;
        store.write_approval(&ApprovalWrite {
            scope: &req.scope,
            entry_id: &req.entry_id,
            expected_revision: req.expected_revision,
            approval_state: "rejected",
            approved_entry_revision: None,
            approved_by: None,
            policy_revision: None,
            rejection_reason: Some(&req.reason),
            updated_at: &stamp,
        })?;
        saved_entry(store, &req.scope, &req.entry_id, now)
    })?;
    to_value(&out)
}

/// `calendar.pause` and `calendar.resume`: move between a pausable state and `paused`, keeping the
/// state to return to in `paused_from`. Each bumps the revision.
pub fn calendar_pause(store: &dyn CalendarStore, input: Value) -> Result<Value, Fail> {
    transition(store, input, "calendar.pause")
}

/// `calendar.resume`: see [`calendar_pause`].
pub fn calendar_resume(store: &dyn CalendarStore, input: Value) -> Result<Value, Fail> {
    transition(store, input, "calendar.resume")
}

fn transition(store: &dyn CalendarStore, input: Value, method: &str) -> Result<Value, Fail> {
    let req: CalendarStateInput = parse_input(input, method)?;
    let now_secs = time::now_secs(req.now.as_deref())?;
    let stamp = time::format_instant(now_secs);
    let out = in_txn(store, || {
        require_channel(store, &req.scope)?;
        let cur = store
            .load_entry(&req.scope, &req.entry_id)?
            .ok_or_else(|| not_found(&req.entry_id))?;
        if let Some(expected) = req.expected_revision {
            check_expected(&cur, expected)?;
        }
        let (state, paused_from): (String, Option<String>) = if method == "calendar.pause" {
            if !PAUSABLE.contains(&cur.state.as_str()) {
                return Err(invalid_transition(format!(
                    "cannot pause an entry in state {}",
                    cur.state
                )));
            }
            ("paused".to_string(), Some(cur.state.clone()))
        } else {
            if cur.state != "paused" {
                return Err(invalid_transition(format!(
                    "cannot resume an entry in state {}",
                    cur.state
                )));
            }
            (
                cur.paused_from
                    .clone()
                    .unwrap_or_else(|| "draft".to_string()),
                None,
            )
        };
        let changed = store.write_state(&StateWrite {
            scope: &req.scope,
            entry_id: &req.entry_id,
            expected_revision: cur.revision,
            state: &state,
            paused_from: paused_from.as_deref(),
            new_revision: cur.revision + 1,
            updated_at: &stamp,
        })?;
        if !changed {
            return Err(conflict(format!(
                "entry {} changed while it was written",
                req.entry_id
            )));
        }
        saved_entry(store, &req.scope, &req.entry_id, now_secs)
    })?;
    to_value(&out)
}

/// `calendar.request_approval`: marks exactly `expected_revision` as waiting for a decision (`approval_state`
/// `pending`). Allowed from `none`, `rejected` and `invalidated` (the rejection reason and any earlier approval
/// stamp are cleared); an `approved` entry is `invalid_transition` (change it, or reject it, first), a pending
/// one is returned unchanged, and a `published` or `failed` entry cannot be asked. The revision does not change.
pub fn calendar_request_approval(store: &dyn CalendarStore, input: Value) -> Result<Value, Fail> {
    let req: CalendarRequestApprovalInput = parse_input(input, "calendar.request_approval")?;
    let now = time::now_secs(req.now.as_deref())?;
    let stamp = time::format_instant(now);
    let out = in_txn(store, || {
        require_channel(store, &req.scope)?;
        let cur = entry_at(store, &req.scope, &req.entry_id, req.expected_revision)?;
        if matches!(cur.state.as_str(), "published" | "failed") {
            return Err(invalid_transition(format!(
                "entry {} is {}: approval cannot be requested",
                req.entry_id, cur.state
            )));
        }
        match cur.approval_state.as_str() {
            "approved" => {
                return Err(invalid_transition(format!(
                    "entry {} is already approved at revision {:?}",
                    req.entry_id, cur.approved_entry_revision
                )))
            }
            "pending" => return present_default(store, cur, now),
            _ => {}
        }
        store.write_approval(&ApprovalWrite {
            scope: &req.scope,
            entry_id: &req.entry_id,
            expected_revision: req.expected_revision,
            approval_state: "pending",
            approved_entry_revision: None,
            approved_by: None,
            policy_revision: None,
            rejection_reason: None,
            updated_at: &stamp,
        })?;
        saved_entry(store, &req.scope, &req.entry_id, now)
    })?;
    to_value(&out)
}

/// `engine.clock`: the host's system clock, so a client need not trust the browser's. Takes no input.
pub fn engine_clock(_input: Value) -> Result<Value, Fail> {
    let secs = time::now_secs(None)?;
    to_value(&EngineClock {
        now_utc: time::format_instant(secs),
        unix_secs: secs,
        source: "system".to_string(),
    })
}

/// `calendar.resolve_failure`: the operator acknowledges a `failed` entry at exactly `expected_revision`, with who
/// (`resolved_by`) and a note. The entry, its state and its publish job are not changed; the failure stops being
/// listed until the entry changes again. Resolving twice replaces the earlier note. Only a `failed` entry can be
/// resolved (`invalid_transition` otherwise).
pub fn calendar_resolve_failure(store: &dyn CalendarStore, input: Value) -> Result<Value, Fail> {
    let resolved_by = input
        .get("resolved_by")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let note = input
        .get("note")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    non_empty(&resolved_by, "resolved_by")?;
    non_empty(&note, "note")?;
    if note.len() > 2000 {
        return Err(bad("note must be at most 2000 bytes"));
    }
    let req: CalendarRequestApprovalInput = parse_input(input, "calendar.resolve_failure")?;
    let now = time::now_secs(req.now.as_deref())?;
    let stamp = time::format_instant(now);
    let out = in_txn(store, || {
        require_channel(store, &req.scope)?;
        let cur = entry_at(store, &req.scope, &req.entry_id, req.expected_revision)?;
        if cur.state != "failed" {
            return Err(invalid_transition(format!(
                "entry {} is {}: only a failed entry has a failure to resolve",
                req.entry_id, cur.state
            )));
        }
        store.put_resolution(&FailureResolution {
            scope: &req.scope,
            entry_id: &req.entry_id,
            entry_revision: cur.revision,
            entry_updated_at: &cur.updated_at,
            resolved_by: &resolved_by,
            note: &note,
            resolved_at: &stamp,
        })?;
        present_default(store, cur, now)
    })?;
    let mut value = to_value(&out)?;
    if let Value::Object(map) = &mut value {
        map.insert(
            "failure_resolution".to_string(),
            serde_json::json!({ "resolved_by": resolved_by, "note": note, "resolved_at": stamp }),
        );
    }
    Ok(value)
}
