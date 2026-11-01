//! Manual draft commands (`draft.put`, `draft.get`, `draft.list`, `draft.approve`, `draft.mark_posted`,
//! `draft.skip`, `draft.mark_stale`) over a host-implemented [`DraftStore`]. A draft is text the operator
//! posts by hand; nothing here posts. Every command checks its scope against a stored channel
//! (`foreign_scope` otherwise).
//!
//! Revisions: put, approve, mark_posted, skip and mark_stale each bump the revision. A stale
//! `expected_revision` is `revision_conflict`. A missing one on an existing draft is `revision_required`.
//! The approval gate and the posted-link host table are in [`crate::drafts`].

use rightkit_social_publish::calendar_time as time;
use rightkit_social_publish::protocol::PLATFORMS;
use rightkit_social_publish::util::{
    bad, in_txn, invalid_transition, non_empty, parse_input, require_channel, to_value, WriteTxn,
};
use serde_json::Value;

use crate::drafts::{host_allowed, https_host};
use crate::protocol::{
    DraftApproveInput, DraftGetInput, DraftListInput, DraftListOutput, DraftMarkPostedInput,
    DraftMarkPostedOutput, DraftMarkStaleInput, DraftPutInput, DraftSkipInput, ManualDraft, Scope,
    DRAFT_KINDS, DRAFT_ONLY_PLATFORMS, DRAFT_STATES,
};
use crate::Fail;

/// Methods that write. A dry run reports these and writes nothing.
pub const WRITE_METHODS: [&str; 5] = [
    "draft.put",
    "draft.approve",
    "draft.mark_posted",
    "draft.skip",
    "draft.mark_stale",
];

/// The fields `draft.put` writes on a draft.
#[derive(Debug, Clone)]
pub struct DraftFields<'a> {
    pub platform: &'a str,
    pub kind: &'a str,
    pub target: Option<&'a str>,
    pub title: Option<&'a str>,
    pub body: &'a str,
    pub disclosure_required: bool,
    pub source_note: Option<&'a str>,
}

/// What the host app provides: the `manual_drafts` table. Every transition is compare-and-swap on the
/// revision (and on the state it moves from) and reports the rows it changed (0 or 1).
pub trait DraftStore: WriteTxn {
    /// One draft, if stored.
    fn load_draft(&self, scope: &Scope, draft_id: &str) -> Result<Option<ManualDraft>, Fail>;
    /// Drafts in scope (optionally one state and one platform), most recently updated first, then by id.
    fn list_drafts(
        &self,
        scope: &Scope,
        state: Option<&str>,
        platform: Option<&str>,
    ) -> Result<Vec<ManualDraft>, Fail>;
    /// Stores a new draft: state `drafted`, revision 1, `created_at` and `updated_at` both `stamp`.
    fn insert_draft(
        &self,
        scope: &Scope,
        draft_id: &str,
        fields: &DraftFields<'_>,
        stamp: &str,
    ) -> Result<(), Fail>;
    /// Replaces the fields of a draft at `expected` revision; the state becomes `drafted`.
    fn update_draft(
        &self,
        scope: &Scope,
        draft_id: &str,
        fields: &DraftFields<'_>,
        revision: i64,
        stamp: &str,
        expected: i64,
    ) -> Result<u64, Fail>;
    /// `drafted` to `approved`.
    fn approve_draft(
        &self,
        scope: &Scope,
        draft_id: &str,
        revision: i64,
        stamp: &str,
        expected: i64,
    ) -> Result<u64, Fail>;
    /// `approved` to `posted_by_operator`, with the link and the posting instant.
    #[allow(clippy::too_many_arguments)]
    fn mark_posted(
        &self,
        scope: &Scope,
        draft_id: &str,
        posted_url: &str,
        posted_at: &str,
        revision: i64,
        stamp: &str,
        expected: i64,
    ) -> Result<u64, Fail>;
    /// `drafted` or `approved` to `skipped`, with the reason.
    fn skip_draft(
        &self,
        scope: &Scope,
        draft_id: &str,
        reason: &str,
        revision: i64,
        stamp: &str,
        expected: i64,
    ) -> Result<u64, Fail>;
    /// `drafted` or `approved` to `stale`.
    fn stale_draft(
        &self,
        scope: &Scope,
        draft_id: &str,
        revision: i64,
        stamp: &str,
        expected: i64,
    ) -> Result<u64, Fail>;
}

fn conflict(msg: impl Into<String>) -> Fail {
    ("revision_conflict", msg.into())
}

fn not_found(draft_id: &str) -> Fail {
    ("not_found", format!("no draft {draft_id} for this scope"))
}

fn changed(draft_id: &str) -> Fail {
    conflict(format!("draft {draft_id} changed while it was written"))
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

fn platform_ok(platform: &str) -> Result<(), Fail> {
    if PLATFORMS.contains(&platform) || DRAFT_ONLY_PLATFORMS.contains(&platform) {
        return Ok(());
    }
    let allowed: Vec<&str> = PLATFORMS
        .iter()
        .chain(DRAFT_ONLY_PLATFORMS.iter())
        .copied()
        .collect();
    Err(bad(format!(
        "platform must be one of {}, got {platform}",
        allowed.join(", ")
    )))
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

/// The clock for this command (`now` when given) and the same instant as stored text.
fn stamp_of(now: Option<&str>) -> Result<(i64, String), Fail> {
    let secs = time::now_secs(now)?;
    Ok((secs, time::format_instant(secs)))
}

fn check_expected(cur: &ManualDraft, expected: i64) -> Result<(), Fail> {
    if cur.revision != expected {
        return Err(conflict(format!(
            "draft {} is at revision {}; expected_revision {expected} is stale",
            cur.draft_id, cur.revision
        )));
    }
    Ok(())
}

/// The deterministic approval gate of a stored draft; see [`crate::drafts::approval_gate`].
fn approval_gate(
    cur: &ManualDraft,
    phrases: &[String],
    max_chars: Option<usize>,
) -> Result<(), Fail> {
    crate::drafts::approval_gate(
        &cur.draft_id,
        &cur.kind,
        &cur.body,
        cur.target.as_deref(),
        cur.disclosure_required,
        phrases,
        max_chars,
    )
}

/// Loads the draft inside the transaction or answers `not_found`.
fn must_load(store: &dyn DraftStore, scope: &Scope, draft_id: &str) -> Result<ManualDraft, Fail> {
    store
        .load_draft(scope, draft_id)?
        .ok_or_else(|| not_found(draft_id))
}

/// `draft.put`: create or update a draft. An edit always leaves the draft `drafted`.
pub fn draft_put(store: &dyn DraftStore, input: Value) -> Result<Value, Fail> {
    let req: DraftPutInput = parse_input(input, "draft.put")?;
    non_empty(&req.draft_id, "draft_id")?;
    platform_ok(&req.platform)?;
    one_of(&req.kind, &DRAFT_KINDS, "kind")?;
    let target = trimmed_opt(req.target.as_deref());
    let title = trimmed_opt(req.title.as_deref());
    let (_, stamp) = stamp_of(req.now.as_deref())?;
    let fields = DraftFields {
        platform: &req.platform,
        kind: &req.kind,
        target: target.as_deref(),
        title: title.as_deref(),
        body: &req.body,
        disclosure_required: req.disclosure_required,
        source_note: req.source_note.as_deref(),
    };

    let saved = in_txn(store, || {
        require_channel(store, &req.scope)?;
        match store.load_draft(&req.scope, &req.draft_id)? {
            None => {
                match req.expected_revision {
                    None | Some(0) => {}
                    Some(n) => {
                        return Err(conflict(format!(
                            "draft {} does not exist (revision 0); expected_revision {n} is stale",
                            req.draft_id
                        )))
                    }
                }
                store.insert_draft(&req.scope, &req.draft_id, &fields, &stamp)?;
            }
            Some(cur) => {
                let expected = req.expected_revision.ok_or_else(|| {
                    (
                        "revision_required",
                        format!(
                            "draft {} exists: expected_revision is required",
                            req.draft_id
                        ),
                    )
                })?;
                check_expected(&cur, expected)?;
                if !matches!(cur.state.as_str(), "drafted" | "approved") {
                    return Err(invalid_transition(format!(
                        "draft {} is {}: only a drafted or approved draft can be edited",
                        req.draft_id, cur.state
                    )));
                }
                let n = store.update_draft(
                    &req.scope,
                    &req.draft_id,
                    &fields,
                    cur.revision + 1,
                    &stamp,
                    expected,
                )?;
                if n != 1 {
                    return Err(changed(&req.draft_id));
                }
            }
        }
        must_load(store, &req.scope, &req.draft_id)
    })?;
    to_value(&saved)
}

/// `draft.get`: one draft in scope.
pub fn draft_get(store: &dyn DraftStore, input: Value) -> Result<Value, Fail> {
    let req: DraftGetInput = parse_input(input, "draft.get")?;
    require_channel(store, &req.scope)?;
    let draft = must_load(store, &req.scope, &req.draft_id)?;
    to_value(&draft)
}

/// `draft.list`: drafts in scope, most recently updated first.
pub fn draft_list(store: &dyn DraftStore, input: Value) -> Result<Value, Fail> {
    let req: DraftListInput = parse_input(input, "draft.list")?;
    if let Some(state) = req.state.as_deref() {
        one_of(state, &DRAFT_STATES, "state")?;
    }
    if let Some(platform) = req.platform.as_deref() {
        platform_ok(platform)?;
    }
    require_channel(store, &req.scope)?;
    let drafts = store.list_drafts(&req.scope, req.state.as_deref(), req.platform.as_deref())?;
    to_value(&DraftListOutput {
        scope: req.scope,
        drafts,
    })
}

/// `draft.approve`: `drafted` to `approved`, through the approval gate.
pub fn draft_approve(store: &dyn DraftStore, input: Value) -> Result<Value, Fail> {
    let req: DraftApproveInput = parse_input(input, "draft.approve")?;
    let (_, stamp) = stamp_of(req.now.as_deref())?;
    let saved = in_txn(store, || {
        require_channel(store, &req.scope)?;
        let cur = must_load(store, &req.scope, &req.draft_id)?;
        check_expected(&cur, req.expected_revision)?;
        if cur.state != "drafted" {
            return Err(invalid_transition(format!(
                "draft {} is {}: only a drafted draft can be approved",
                req.draft_id, cur.state
            )));
        }
        approval_gate(&cur, &req.disclosure_phrases, req.max_chars)?;
        let n = store.approve_draft(
            &req.scope,
            &req.draft_id,
            cur.revision + 1,
            &stamp,
            req.expected_revision,
        )?;
        if n != 1 {
            return Err(changed(&req.draft_id));
        }
        must_load(store, &req.scope, &req.draft_id)
    })?;
    to_value(&saved)
}

/// `draft.mark_posted`: `approved` to `posted_by_operator`, with the link the operator posted.
pub fn draft_mark_posted(store: &dyn DraftStore, input: Value) -> Result<Value, Fail> {
    let req: DraftMarkPostedInput = parse_input(input, "draft.mark_posted")?;
    let url = req.posted_url.trim().to_string();
    non_empty(&url, "posted_url")?;
    let (_, stamp) = stamp_of(req.now.as_deref())?;
    let posted_at =
        instant_opt(req.posted_at.as_deref(), "posted_at")?.unwrap_or_else(|| stamp.clone());
    let (saved, host_check, host_note) = in_txn(store, || {
        require_channel(store, &req.scope)?;
        let cur = must_load(store, &req.scope, &req.draft_id)?;
        check_expected(&cur, req.expected_revision)?;
        if cur.state != "approved" {
            return Err(invalid_transition(format!(
                "draft {} is {}: only an approved draft can be marked posted",
                req.draft_id, cur.state
            )));
        }
        let host = https_host(&url)?;
        let (host_check, host_note) = host_allowed(&cur.platform, &host)?;
        let n = store.mark_posted(
            &req.scope,
            &req.draft_id,
            &url,
            &posted_at,
            cur.revision + 1,
            &stamp,
            req.expected_revision,
        )?;
        if n != 1 {
            return Err(changed(&req.draft_id));
        }
        let saved = must_load(store, &req.scope, &req.draft_id)?;
        Ok((saved, host_check, host_note))
    })?;
    to_value(&DraftMarkPostedOutput {
        draft: saved,
        host_check: host_check.to_string(),
        host_note,
    })
}

/// `draft.skip`: `drafted` or `approved` to `skipped`; the reason is required.
pub fn draft_skip(store: &dyn DraftStore, input: Value) -> Result<Value, Fail> {
    let req: DraftSkipInput = parse_input(input, "draft.skip")?;
    let reason = req.reason.trim().to_string();
    if reason.is_empty() {
        return Err(bad("reason must not be empty"));
    }
    let (_, stamp) = stamp_of(req.now.as_deref())?;
    let saved = in_txn(store, || {
        require_channel(store, &req.scope)?;
        let cur = must_load(store, &req.scope, &req.draft_id)?;
        check_expected(&cur, req.expected_revision)?;
        if !matches!(cur.state.as_str(), "drafted" | "approved") {
            return Err(invalid_transition(format!(
                "draft {} is {}: only a drafted or approved draft can be skipped",
                req.draft_id, cur.state
            )));
        }
        let n = store.skip_draft(
            &req.scope,
            &req.draft_id,
            &reason,
            cur.revision + 1,
            &stamp,
            req.expected_revision,
        )?;
        if n != 1 {
            return Err(changed(&req.draft_id));
        }
        must_load(store, &req.scope, &req.draft_id)
    })?;
    to_value(&saved)
}

/// `draft.mark_stale`: `drafted` or `approved` to `stale`.
pub fn draft_mark_stale(store: &dyn DraftStore, input: Value) -> Result<Value, Fail> {
    let req: DraftMarkStaleInput = parse_input(input, "draft.mark_stale")?;
    let (_, stamp) = stamp_of(req.now.as_deref())?;
    let saved = in_txn(store, || {
        require_channel(store, &req.scope)?;
        let cur = must_load(store, &req.scope, &req.draft_id)?;
        check_expected(&cur, req.expected_revision)?;
        if !matches!(cur.state.as_str(), "drafted" | "approved") {
            return Err(invalid_transition(format!(
                "draft {} is {}: only a drafted or approved draft can be marked stale",
                req.draft_id, cur.state
            )));
        }
        let n = store.stale_draft(
            &req.scope,
            &req.draft_id,
            cur.revision + 1,
            &stamp,
            req.expected_revision,
        )?;
        if n != 1 {
            return Err(changed(&req.draft_id));
        }
        must_load(store, &req.scope, &req.draft_id)
    })?;
    to_value(&saved)
}
