//! Accounts: the store of connected platform accounts and their readiness. The host app keeps the table
//! and implements [`AccountStore`] over it. Every row is
//! keyed by venture, channel, account and platform, and every command checks its scope against a
//! stored channel. A channel holds one account per platform (the fixture `accounts` map).
//!
//! The daemon owns the real token, entitlement and cooldown state. Until it reports them, this store
//! holds `unknown` or `not_started`, and readiness is derived from the stored fields only. Nothing
//! here touches the network or a platform.
//!
//! Revisions: `account.put`, `account.pause` and `account.resume` each bump the revision. A stale
//! `expected_revision` is `revision_conflict`. Pause state is changed only by pause and resume.
//! Cooldown: a quota cooldown is active while its instant is strictly after `now`.
//!
//! `account.disconnect` ends the connection: the state becomes `not_connected`, the
//! token expiry is cleared, the revision bumps, and the account's queued and waiting publish jobs are cancelled in the
//! same transaction. The engine never stores a platform token (a token lives only in a command's input), so there is
//! no stored token to erase; clearing the state and the expiry is the whole revocation on this side. A job that may
//! already have a post on the platform (running, or holding a remote id) is not cancelled; it is listed in
//! `kept_jobs` for the daemon to reconcile. Disconnecting does not delete data: `deletion.request` does.
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::calendar_time as time;
use crate::protocol::{
    AccountDisconnectInput, AccountDisconnectOutput, AccountGetInput, AccountImportInput,
    AccountImportOutput, AccountListInput, AccountListOutput, AccountPauseInput, AccountPutInput,
    AccountReadiness, AccountRecord, AccountResumeInput, Scope, CONNECTION_STATES,
    ENTITLEMENT_STATES, INSTAGRAM_LOGIN_ROUTES, PLATFORMS,
};
use crate::util::{bad, in_txn, invalid_transition, non_empty, require_channel, WriteTxn};
use crate::Fail;

/// Methods that write. A dry run reports these and writes nothing.
pub const WRITE_METHODS: [&str; 5] = [
    "account.import",
    "account.put",
    "account.pause",
    "account.resume",
    "account.disconnect",
];

/// The fields `account.put` replaces on an account (everything but the pause state and the revision).
#[derive(Debug, Clone)]
pub struct AccountFields {
    pub handle: String,
    pub display_name: String,
    pub account_kind: String,
    pub login_route: Option<String>,
    pub connection_state: String,
    pub token_expires_at: Option<String>,
    pub sandbox_mechanics: String,
    pub review_submission: String,
    pub audience_visible: String,
    pub quota_cooldown_until: Option<String>,
}

/// A publish job a disconnect looks at: not yet finished and not waiting on a person.
#[derive(Debug, Clone)]
pub struct PendingJob {
    pub job_id: String,
    /// The job's state (`queued`, `running` or `waiting`).
    pub state: String,
    pub inflight: Option<String>,
    /// The stored `remote` JSON text.
    pub remote_json: String,
}

/// What the host app provides: the accounts table, keyed by venture, channel, account and platform, and
/// the cancel of a publish job. Every write is compare-and-swap on the revision and reports the rows it
/// changed (0 or 1), so the module can answer `revision_conflict`.
pub trait AccountStore: WriteTxn {
    /// One account (readiness left at its empty default), if stored.
    fn load_account(&self, scope: &Scope, platform: &str) -> Result<Option<AccountRecord>, Fail>;
    /// Every account matching the given venture, channel and account (each `None` matches all), ordered
    /// by venture, platform, handle, channel, account.
    fn list_accounts(
        &self,
        venture: Option<&str>,
        channel: Option<&str>,
        account: Option<&str>,
    ) -> Result<Vec<AccountRecord>, Fail>;
    /// Seeds one account (not connected, nothing started, revision 1, empty display name, `unknown` kind,
    /// `handle`) unless it exists; the rows written (0 when it already existed).
    fn import_account(
        &self,
        scope: &Scope,
        platform: &str,
        handle: &str,
        stamp: &str,
    ) -> Result<u64, Fail>;
    /// Inserts a new account at revision 1, not paused.
    fn insert_account(
        &self,
        scope: &Scope,
        platform: &str,
        fields: &AccountFields,
        stamp: &str,
    ) -> Result<(), Fail>;
    /// Replaces the fields of the account at `expected` revision and sets `revision`.
    fn update_account(
        &self,
        scope: &Scope,
        platform: &str,
        fields: &AccountFields,
        revision: i64,
        stamp: &str,
        expected: i64,
    ) -> Result<u64, Fail>;
    /// Pauses the account at `expected` revision with `reason` at `stamp`, setting `revision`.
    fn pause_account(
        &self,
        scope: &Scope,
        platform: &str,
        reason: &str,
        stamp: &str,
        revision: i64,
        expected: i64,
    ) -> Result<u64, Fail>;
    /// Clears the pause of the account at `expected` revision, setting `revision`.
    fn resume_account(
        &self,
        scope: &Scope,
        platform: &str,
        revision: i64,
        stamp: &str,
        expected: i64,
    ) -> Result<u64, Fail>;
    /// Sets the account `not_connected`, clears its token expiry, sets `revision`; at `expected` revision.
    fn disconnect_account(
        &self,
        scope: &Scope,
        platform: &str,
        revision: i64,
        stamp: &str,
        expected: i64,
    ) -> Result<u64, Fail>;
    /// The account's `queued`, `running` and `waiting` publish jobs, oldest first (by creation, then id).
    fn pending_jobs(&self, scope: &Scope, platform: &str) -> Result<Vec<PendingJob>, Fail>;
    /// Cancels a job that is not finished, on the caller's transaction, as `publish.cancel` does, and bumps
    /// its revision.
    fn cancel_job(&self, job_id: &str, code: &str, message: &str, stamp: &str) -> Result<(), Fail>;
}

fn encoded(value: serde_json::Result<Value>) -> Result<Value, Fail> {
    value.map_err(|e| ("internal", e.to_string()))
}

fn conflict(msg: impl Into<String>) -> Fail {
    ("revision_conflict", msg.into())
}

fn changed(platform: &str) -> Fail {
    conflict(format!("account {platform} changed while it was written"))
}

fn not_found(platform: &str) -> Fail {
    ("not_found", format!("no account {platform} for this scope"))
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

fn check_expected(cur: &AccountRecord, expected: i64) -> Result<(), Fail> {
    if cur.revision != expected {
        return Err(conflict(format!(
            "account {} is at revision {}; expected_revision {expected} is stale",
            cur.platform, cur.revision
        )));
    }
    Ok(())
}

/// Readiness from the stored fields at `now`. Blocker codes come in a fixed order. `can_publish` is
/// true exactly when there are no blockers: connected, not paused, no active cooldown, and
/// sandbox mechanics passed.
fn readiness_of(rec: &AccountRecord, now: i64) -> Result<AccountReadiness, Fail> {
    let mut blockers: Vec<String> = Vec::new();
    match rec.connection_state.as_str() {
        "not_connected" => blockers.push("not_connected".to_string()),
        "reauth_required" => blockers.push("reauth_required".to_string()),
        _ => {}
    }
    if rec.paused {
        blockers.push("paused".to_string());
    }
    if let Some(until) = &rec.quota_cooldown_until {
        if time::parse_instant(until, "quota_cooldown_until")? > now {
            blockers.push("cooldown".to_string());
        }
    }
    if rec.sandbox_mechanics != "passed" {
        blockers.push("mechanics_not_passed".to_string());
    }
    if rec.connection_state == "unknown" {
        blockers.push("connection_unknown".to_string());
    }
    Ok(AccountReadiness {
        can_publish: blockers.is_empty(),
        blockers,
    })
}

fn with_readiness(mut rec: AccountRecord, now: i64) -> Result<AccountRecord, Fail> {
    rec.readiness = readiness_of(&rec, now)?;
    Ok(rec)
}

fn fields_of(
    req: &AccountPutInput,
    token_expires_at: Option<String>,
    quota_cooldown_until: Option<String>,
) -> AccountFields {
    AccountFields {
        handle: req.handle.clone(),
        display_name: req.display_name.clone(),
        account_kind: req.account_kind.clone(),
        login_route: req.login_route.clone(),
        connection_state: req.connection_state.clone(),
        token_expires_at,
        sandbox_mechanics: req.sandbox_mechanics.clone(),
        review_submission: req.review_submission.clone(),
        audience_visible: req.audience_visible.clone(),
        quota_cooldown_until,
    }
}

/// `account.put`: create or update one account. Every field this command sets is replaced; the pause
/// state is not. Each write bumps the revision (a new account starts at revision 1).
pub fn account_put(store: &dyn AccountStore, input: Value) -> Result<Value, Fail> {
    let req: AccountPutInput =
        serde_json::from_value(input).map_err(|e| bad(format!("account.put input: {e}")))?;
    non_empty(&req.platform, "platform")?;
    one_of(&req.platform, &PLATFORMS, "platform")?;
    non_empty(&req.handle, "handle")?;
    non_empty(&req.account_kind, "account_kind")?;
    one_of(
        &req.connection_state,
        &CONNECTION_STATES,
        "connection_state",
    )?;
    one_of(
        &req.sandbox_mechanics,
        &ENTITLEMENT_STATES,
        "sandbox_mechanics",
    )?;
    one_of(
        &req.review_submission,
        &ENTITLEMENT_STATES,
        "review_submission",
    )?;
    one_of(
        &req.audience_visible,
        &ENTITLEMENT_STATES,
        "audience_visible",
    )?;
    if let Some(route) = req.login_route.as_deref() {
        if req.platform != "instagram" || !INSTAGRAM_LOGIN_ROUTES.contains(&route) {
            return Err(bad(
                "login_route is set only on instagram, as instagram_login or facebook_login",
            ));
        }
    }
    let token_expires_at = instant_opt(req.token_expires_at.as_deref(), "token_expires_at")?;
    let quota_cooldown_until =
        instant_opt(req.quota_cooldown_until.as_deref(), "quota_cooldown_until")?;
    let (now_secs, stamp) = stamp_of(req.now.as_deref())?;
    let fields = fields_of(&req, token_expires_at, quota_cooldown_until);

    let saved = in_txn(store, || {
        require_channel(store, &req.scope)?;
        match store.load_account(&req.scope, &req.platform)? {
            None => {
                match req.expected_revision {
                    None | Some(0) => {}
                    Some(n) => {
                        return Err(conflict(format!(
                        "account {} does not exist (revision 0); expected_revision {n} is stale",
                        req.platform
                    )))
                    }
                }
                store.insert_account(&req.scope, &req.platform, &fields, &stamp)?;
            }
            Some(cur) => {
                let expected = req.expected_revision.ok_or_else(|| {
                    (
                        "revision_required",
                        format!(
                            "account {} exists: expected_revision is required",
                            req.platform
                        ),
                    )
                })?;
                check_expected(&cur, expected)?;
                let n = store.update_account(
                    &req.scope,
                    &req.platform,
                    &fields,
                    cur.revision + 1,
                    &stamp,
                    expected,
                )?;
                if n != 1 {
                    return Err(changed(&req.platform));
                }
            }
        }
        store
            .load_account(&req.scope, &req.platform)?
            .ok_or_else(|| not_found(&req.platform))
    })?;
    encoded(serde_json::to_value(with_readiness(saved, now_secs)?))
}

/// `account.get`: one account in scope, with readiness at `now`.
pub fn account_get(store: &dyn AccountStore, input: Value) -> Result<Value, Fail> {
    let req: AccountGetInput =
        serde_json::from_value(input).map_err(|e| bad(format!("account.get input: {e}")))?;
    let now = time::now_secs(req.now.as_deref())?;
    require_channel(store, &req.scope)?;
    let rec = store
        .load_account(&req.scope, &req.platform)?
        .ok_or_else(|| not_found(&req.platform))?;
    encoded(serde_json::to_value(with_readiness(rec, now)?))
}

/// `account.list`: every account in one scope, or all of them when no scope is given. Ordered by
/// venture, platform, handle (then channel and account, so the order is total).
pub fn account_list(store: &dyn AccountStore, input: Value) -> Result<Value, Fail> {
    // A call with no input (the CLI's bare `account.list`) reads as an empty request.
    let input = if input.is_null() { json!({}) } else { input };
    let req: AccountListInput =
        serde_json::from_value(input).map_err(|e| bad(format!("account.list input: {e}")))?;
    let now = time::now_secs(req.now.as_deref())?;
    let (venture, channel, account) = match &req.scope {
        Some(scope) => {
            require_channel(store, scope)?;
            (
                Some(scope.venture.clone()),
                Some(scope.channel.clone()),
                Some(scope.account.clone()),
            )
        }
        None => (None, None, None),
    };
    let mut accounts = Vec::new();
    for rec in store.list_accounts(venture.as_deref(), channel.as_deref(), account.as_deref())? {
        accounts.push(with_readiness(rec, now)?);
    }
    encoded(serde_json::to_value(AccountListOutput { accounts }))
}

/// `account.pause`: stops the account from publishing. `reason` is required. Pausing a paused
/// account is `invalid_transition`.
pub fn account_pause(store: &dyn AccountStore, input: Value) -> Result<Value, Fail> {
    let req: AccountPauseInput =
        serde_json::from_value(input).map_err(|e| bad(format!("account.pause input: {e}")))?;
    let reason = req.reason.trim().to_string();
    if reason.is_empty() {
        return Err(bad("reason must not be empty"));
    }
    let (now_secs, stamp) = stamp_of(req.now.as_deref())?;
    let saved = in_txn(store, || {
        require_channel(store, &req.scope)?;
        let cur = store
            .load_account(&req.scope, &req.platform)?
            .ok_or_else(|| not_found(&req.platform))?;
        check_expected(&cur, req.expected_revision)?;
        if cur.paused {
            return Err(invalid_transition(format!(
                "account {} is already paused",
                req.platform
            )));
        }
        let n = store.pause_account(
            &req.scope,
            &req.platform,
            &reason,
            &stamp,
            cur.revision + 1,
            cur.revision,
        )?;
        if n != 1 {
            return Err(changed(&req.platform));
        }
        store
            .load_account(&req.scope, &req.platform)?
            .ok_or_else(|| not_found(&req.platform))
    })?;
    encoded(serde_json::to_value(with_readiness(saved, now_secs)?))
}

/// `account.resume`: clears the pause. Resuming an account that is not paused is `invalid_transition`.
pub fn account_resume(store: &dyn AccountStore, input: Value) -> Result<Value, Fail> {
    let req: AccountResumeInput =
        serde_json::from_value(input).map_err(|e| bad(format!("account.resume input: {e}")))?;
    let (now_secs, stamp) = stamp_of(req.now.as_deref())?;
    let saved = in_txn(store, || {
        require_channel(store, &req.scope)?;
        let cur = store
            .load_account(&req.scope, &req.platform)?
            .ok_or_else(|| not_found(&req.platform))?;
        check_expected(&cur, req.expected_revision)?;
        if !cur.paused {
            return Err(invalid_transition(format!(
                "account {} is not paused",
                req.platform
            )));
        }
        let n = store.resume_account(
            &req.scope,
            &req.platform,
            cur.revision + 1,
            &stamp,
            cur.revision,
        )?;
        if n != 1 {
            return Err(changed(&req.platform));
        }
        store
            .load_account(&req.scope, &req.platform)?
            .ok_or_else(|| not_found(&req.platform))
    })?;
    encoded(serde_json::to_value(with_readiness(saved, now_secs)?))
}

/// Cancels the account's queued and waiting publish jobs and returns `(cancelled, kept)` job ids. A job is kept
/// when it may already have a post on the platform: running, something in flight, or a remote post id recorded.
fn cancel_pending_jobs(
    store: &dyn AccountStore,
    scope: &Scope,
    platform: &str,
    stamp: &str,
) -> Result<(Vec<String>, Vec<String>), Fail> {
    let rows = store.pending_jobs(scope, platform)?;
    let mut cancelled = Vec::new();
    let mut kept = Vec::new();
    for PendingJob {
        job_id,
        state,
        inflight,
        remote_json,
    } in rows
    {
        let remote: Value = serde_json::from_str(&remote_json)
            .map_err(|e| ("internal", format!("job {job_id} remote_json: {e}")))?;
        let has_post = remote.get("post_id").map(|v| !v.is_null()).unwrap_or(false);
        if state == "running" || inflight.is_some() || has_post {
            kept.push(job_id);
            continue;
        }
        store.cancel_job(
            &job_id,
            "cancelled_by_operator",
            "account disconnected",
            stamp,
        )?;
        cancelled.push(job_id);
    }
    Ok((cancelled, kept))
}

/// `account.disconnect`: ends the connection and cancels the account's pending publish jobs, in one transaction.
/// Disconnecting an account that is already `not_connected` is `invalid_transition`.
pub fn account_disconnect(store: &dyn AccountStore, input: Value) -> Result<Value, Fail> {
    let req: AccountDisconnectInput =
        serde_json::from_value(input).map_err(|e| bad(format!("account.disconnect input: {e}")))?;
    let (now_secs, stamp) = stamp_of(req.now.as_deref())?;
    let (saved, cancelled_jobs, kept_jobs) = in_txn(store, || {
        require_channel(store, &req.scope)?;
        let cur = store
            .load_account(&req.scope, &req.platform)?
            .ok_or_else(|| not_found(&req.platform))?;
        check_expected(&cur, req.expected_revision)?;
        if cur.connection_state == "not_connected" {
            return Err(invalid_transition(format!(
                "account {} is already disconnected",
                req.platform
            )));
        }
        let n = store.disconnect_account(
            &req.scope,
            &req.platform,
            cur.revision + 1,
            &stamp,
            cur.revision,
        )?;
        if n != 1 {
            return Err(changed(&req.platform));
        }
        let (cancelled, kept) = cancel_pending_jobs(store, &req.scope, &req.platform, &stamp)?;
        let saved = store
            .load_account(&req.scope, &req.platform)?
            .ok_or_else(|| not_found(&req.platform))?;
        Ok((saved, cancelled, kept))
    })?;
    encoded(serde_json::to_value(AccountDisconnectOutput {
        account: with_readiness(saved, now_secs)?,
        cancelled_jobs,
        kept_jobs,
    }))
}

fn text_of(v: &Value, key: &str) -> Result<String, Fail> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| bad(format!("scope.json: missing or empty string `{key}`")))
}

/// Reads a fixture `scope.json`: its scope, and its `accounts` map of platform to account id, sorted
/// by platform. Every value must be a non-empty string.
fn read_scope_accounts(path: &Path) -> Result<(Scope, Vec<(String, String)>), Fail> {
    let raw =
        std::fs::read_to_string(path).map_err(|e| bad(format!("read {}: {e}", path.display())))?;
    let doc: Value = serde_json::from_str(&raw)
        .map_err(|e| bad(format!("{} is not JSON: {e}", path.display())))?;
    let declared = doc.get("scope").unwrap_or(&Value::Null);
    let scope = Scope {
        venture: text_of(declared, "venture")?,
        channel: text_of(declared, "channel")?,
        account: text_of(declared, "account")?,
    };
    let map = doc
        .get("accounts")
        .and_then(Value::as_object)
        .ok_or_else(|| bad("scope.json: `accounts` must be an object of platform to account id"))?;
    let mut out = Vec::with_capacity(map.len());
    for (platform, id) in map {
        if platform.trim().is_empty() {
            return Err(bad("scope.json: accounts has an empty platform name"));
        }
        let id = id
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                bad(format!(
                    "scope.json: accounts.{platform} must be a non-empty account id"
                ))
            })?;
        out.push((platform.clone(), id.to_string()));
    }
    out.sort();
    Ok((scope, out))
}

/// `account.import`: seeds one account per entry of a fixture's `accounts` map. Idempotent: an
/// existing row keeps its state. The channel must already be stored (`channel.import`).
pub fn account_import(store: &dyn AccountStore, input: Value) -> Result<Value, Fail> {
    let req: AccountImportInput =
        serde_json::from_value(input).map_err(|e| bad(format!("account.import input: {e}")))?;
    // Callers pass either separator; Windows verbatim paths (`\\?\`) accept only `\`.
    let dir = PathBuf::from(req.dir.replace(['/', '\\'], std::path::MAIN_SEPARATOR_STR));
    let (scope, accounts) = read_scope_accounts(&dir.join("scope.json"))?;
    // Refused before any write: one unknown platform in the map refuses the whole import.
    for (platform, _) in &accounts {
        one_of(platform, &PLATFORMS, "scope.json accounts platform")?;
    }
    let (now_secs, stamp) = stamp_of(req.now.as_deref())?;

    let (created, records) = in_txn(store, || {
        require_channel(store, &scope)?;
        let mut created: u64 = 0;
        for (platform, handle) in &accounts {
            created += store.import_account(&scope, platform, handle, &stamp)?;
        }
        let mut records = Vec::with_capacity(accounts.len());
        for (platform, _) in &accounts {
            let rec = store
                .load_account(&scope, platform)?
                .ok_or_else(|| not_found(platform))?;
            records.push(with_readiness(rec, now_secs)?);
        }
        Ok((created, records))
    })?;
    let existing = accounts.len() as u64 - created;
    encoded(serde_json::to_value(AccountImportOutput {
        scope,
        created,
        existing,
        accounts: records,
    }))
}
