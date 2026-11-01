//! DM automation: a keyword comment becomes ONE private reply, per account, from the signed inbox. The
//! tables and the one platform call belong to the host: [`AutomationStore`] holds the rules, the rate
//! setting, the opt-outs, the outcomes and the draft queue; [`Transport`] makes the platform request.
//!
//! What may be sent. Only an inbox `comment` event that is bound to an account, still `open`, from a
//! commenter other than the account itself, whose text matches a stored keyword rule of that account and
//! platform (case-insensitive, whole words, in order). A comment that matches no rule is never answered: it
//! is queued as a draft (no text; a person or skill writes it later) and nothing is sent.
//!
//! Never twice. The outcomes hold at most one row per inbox event and at most one per (platform, account,
//! comment id, recipient id), so a replay, a reordered redelivery, a second event row for the same comment,
//! and a restart all land on the stored row. A send is claimed FIRST: the `sending` row is committed before
//! the platform call, so a crash between the call and its answer leaves a row that is never resent. The
//! claim is removed only when the platform definitely did not take the request (429, 401, a connect
//! failure, an unbuildable request); an answer that may or may not have been acted on (timeout, 5xx, a
//! broken read) is the final outcome `unknown` and is never retried.
//!
//! The window. The 24 hour messaging window ([`DM_WINDOW_SECS`]) is counted from the EVENT's own time,
//! never from when it was received or processed, and is checked at send time against the caller's `now`.
//! Past it, or with an event time that cannot be read or lies in the future, the outcome is `refused` with a
//! named reason (`window_expired`, `event_time_unreadable`, `event_time_in_future`); nothing is sent.
//!
//! The rate limit. Per account, at most `per_hour` claimed sends in any rolling hour (`dm.rate_put`, default
//! [`DEFAULT_PER_HOUR`], a proposed value). Over it the event is `deferred` (not recorded) and is picked up
//! by a later sweep while still inside the window.
//!
//! The token is a field of the call input: used for the one request, never stored, returned or logged; the
//! platform error text is scrubbed of it by the transport.

use rightkit_http::percent_encode;
use rightkit_social_analytics::links::LinksStore;
use rightkit_social_publish::calendar_time as time;
use rightkit_social_publish::http::Reply;
use rightkit_social_publish::util::{bad, in_txn, parse_input, require_channel};
use serde_json::{json, Value};

use crate::inbox::require_human;
use crate::keywords::{keyword_matches, words};
use crate::protocol::{
    DmDraftDismissInput, DmDraftListInput, DmOptOutInput, DmOutcomeListInput, DmProcessInput,
    DmRatePutInput, DmRuleDeleteInput, DmRuleListInput, DmRulePutInput, DmSweepInput, Scope,
    DM_PLATFORMS, DM_WINDOW_SECS,
};
use crate::webhook::event_secs;
use crate::Fail;

/// Methods that write. A dry run reports these with [`dry_run_input`], never the raw input.
pub const WRITE_METHODS: &[&str] = &[
    "dm.rule_put",
    "dm.rule_delete",
    "dm.rate_put",
    "dm.opt_out",
    "dm.process",
    "dm.sweep",
    "dm.draft_dismiss",
];

/// Every method this module routes.
pub const METHODS: &[&str] = &[
    "dm.rule_put",
    "dm.rule_list",
    "dm.rule_delete",
    "dm.rate_put",
    "dm.opt_out",
    "dm.process",
    "dm.sweep",
    "dm.outcome_list",
    "dm.draft_list",
    "dm.draft_dismiss",
];

/// Private replies one account may claim in any rolling hour unless `dm.rate_put` says otherwise. Proposed, unverified.
pub const DEFAULT_PER_HOUR: u32 = 60;
/// Longest reply text a rule may hold, in characters (the link line is added on top; the sum stays under the cap).
const MAX_REPLY_CHARS: usize = 700;
/// Longest message sent, in characters.
const MAX_MESSAGE_CHARS: usize = 1000;
const MAX_KEYWORD_CHARS: usize = 64;
const MAX_RULES_PER_ACCOUNT: i64 = 50;
const MAX_TOKEN_CHARS: usize = 4096;
const SLUG_MAX: usize = 64;
const DEFAULT_LIMIT: u32 = 50;
const MAX_LIMIT: u32 = 200;
/// An event time this far ahead of `now` is treated as unreadable clock data, not as inside the window.
const FUTURE_SKEW_SECS: i64 = 300;

/// A stored inbox event, as automation reads it.
#[derive(Debug, Clone)]
pub struct DmEvent {
    pub event_id: String,
    pub platform: String,
    /// The bound account's scope; `None` for an unmatched event.
    pub scope: Option<Scope>,
    pub recipient: Option<String>,
    pub kind: String,
    pub comment_id: Option<String>,
    pub sender: Option<String>,
    pub text: Option<String>,
    pub time: Option<String>,
    pub state: String,
}

/// A stored outcome, as a replay reads it back.
#[derive(Debug, Clone)]
pub struct DmPrev {
    pub outcome: String,
    pub reason: Option<String>,
    pub rule_id: Option<String>,
    pub link_id: Option<String>,
    pub remote_id: Option<String>,
}

/// One stored keyword rule.
#[derive(Debug, Clone)]
pub struct DmRule {
    pub rule_id: String,
    pub platform: String,
    pub keyword: String,
    pub reply_text: String,
    pub link_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Everything an outcome row is built from.
#[derive(Debug, Clone)]
pub struct NewOutcome<'a> {
    pub ev: &'a DmEvent,
    pub scope: &'a Scope,
    pub comment_id: &'a str,
    pub recipient: &'a str,
    pub outcome: &'a str,
    pub reason: Option<&'a str>,
    pub rule_id: Option<&'a str>,
    pub link_id: Option<&'a str>,
    pub detail: &'a str,
    pub claimed_at: Option<&'a str>,
    pub decided_at: &'a str,
}

/// A rule to store (create, or replace the rule with this id and keep its `created_at`).
#[derive(Debug, Clone)]
pub struct NewRule<'a> {
    pub scope: &'a Scope,
    pub rule_id: &'a str,
    pub platform: &'a str,
    pub keyword: &'a str,
    pub reply_text: &'a str,
    pub link_id: Option<&'a str>,
    pub stamp: &'a str,
}

/// A queued draft to store (state `pending`, empty body); ignored when the id exists.
#[derive(Debug, Clone)]
pub struct NewDraft<'a> {
    pub draft_id: &'a str,
    pub platform: &'a str,
    pub scope: &'a Scope,
    pub comment_id: &'a str,
    pub sender_id: &'a str,
    pub comment_text: &'a str,
    pub created_at: &'a str,
}

/// The final state of a claimed send.
#[derive(Debug, Clone)]
pub struct Finish<'a> {
    pub event_id: &'a str,
    pub outcome: &'a str,
    pub reason: Option<&'a str>,
    pub remote_id: Option<&'a str>,
    pub detail: &'a str,
    pub decided_at: &'a str,
}

/// What the host app provides for DM automation, on top of the tracked-links tables ([`LinksStore`], used
/// when a rule carries a link). Every method is one read or one write on the host's own database, on the
/// caller's transaction (outside one, each write commits by itself).
pub trait AutomationStore: LinksStore {
    /// `role_forbidden` unless the host runs in the role that may send.
    fn require_role(&self) -> Result<(), Fail>;
    /// The path prefix of the host's public link redirect route, ending in `/`; the link id follows it.
    fn public_link_prefix(&self) -> &str;
    /// One stored inbox event.
    fn dm_event(&self, event_id: &str) -> Result<Option<DmEvent>, Fail>;
    /// The outcome stored for this event.
    fn prev_by_event(&self, event_id: &str) -> Result<Option<DmPrev>, Fail>;
    /// The outcome stored for this platform, account, comment and recipient.
    fn prev_by_key(
        &self,
        platform: &str,
        scope: &Scope,
        comment_id: &str,
        recipient: &str,
    ) -> Result<Option<DmPrev>, Fail>;
    /// Stores one outcome row; `false` (nothing written) when the event or the (platform, account, comment,
    /// recipient) key already has one.
    fn insert_outcome(&self, row: &NewOutcome<'_>) -> Result<bool, Fail>;
    /// Removes the `sending` claim of an event.
    fn delete_claim(&self, event_id: &str) -> Result<(), Fail>;
    /// Moves a `sending` row to its final state.
    fn finish_claim(&self, finish: &Finish<'_>) -> Result<(), Fail>;
    /// Stores one draft.
    fn insert_draft(&self, draft: &NewDraft<'_>) -> Result<(), Fail>;
    /// Whether this recipient opted out of the account.
    fn opted_out(&self, scope: &Scope, recipient: &str) -> Result<bool, Fail>;
    /// Records an opt-out; `false` when it was already recorded.
    fn insert_opt_out(
        &self,
        scope: &Scope,
        recipient: &str,
        by: &str,
        at: &str,
    ) -> Result<bool, Fail>;
    /// The account's rules (of `platform` when given), ordered by creation then id.
    fn rules(&self, scope: &Scope, platform: Option<&str>) -> Result<Vec<DmRule>, Fail>;
    /// When the rule was created, if it exists.
    fn rule_created_at(&self, scope: &Scope, rule_id: &str) -> Result<Option<String>, Fail>;
    /// How many rules the account holds.
    fn rule_count(&self, scope: &Scope) -> Result<i64, Fail>;
    /// Creates the rule, or replaces its platform, keyword, reply, link and update time.
    fn upsert_rule(&self, rule: &NewRule<'_>) -> Result<(), Fail>;
    /// Deletes a rule; `false` when there was none.
    fn delete_rule(&self, scope: &Scope, rule_id: &str) -> Result<bool, Fail>;
    /// Sets the account's hourly limit.
    fn put_rate(&self, scope: &Scope, per_hour: u32, stamp: &str) -> Result<(), Fail>;
    /// The account's stored hourly limit, if set.
    fn rate(&self, scope: &Scope) -> Result<Option<i64>, Fail>;
    /// Claims after `since` for the account: their count and the oldest claim time.
    fn claims_since(&self, scope: &Scope, since: &str) -> Result<(i64, Option<String>), Fail>;
    /// Open comment events of the account with no outcome, oldest received first, at most `limit`.
    fn unprocessed_comment_ids(&self, scope: &Scope, limit: i64) -> Result<Vec<String>, Fail>;
    /// Outcome rows as JSON objects (`event_id`, `platform`, `comment_id`, `recipient_id`, `outcome`,
    /// `reason`, `rule_id`, `link_id`, `remote_id`, `detail`, `event_time`, `decided_at`), newest decision
    /// first then by event id.
    fn outcome_rows(
        &self,
        scope: &Scope,
        outcome: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Value>, Fail>;
    /// Draft rows as JSON objects (`draft_id`, `platform`, `comment_id`, `sender_id`, `comment_text`, `body`,
    /// `state`, `state_by`, `state_at`, `created_at`), oldest first then by id.
    fn draft_rows(
        &self,
        scope: &Scope,
        state: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Value>, Fail>;
    /// Marks a draft dismissed; `false` when the account has no such draft.
    fn dismiss_draft(
        &self,
        scope: &Scope,
        draft_id: &str,
        by: &str,
        at: &str,
    ) -> Result<bool, Fail>;
}

/// The one platform call of a send, kept by the host (its HTTP client, endpoint settings and error
/// scrubbing).
pub trait Transport {
    /// The validated base URL for a platform: `given` (the command's `base_url`), else the host's setting.
    fn base_url(&self, platform: &str, given: Option<&str>) -> Result<String, Fail>;
    /// POSTs `body` as JSON to `url` with `token` as the bearer credential and classifies the answer. The
    /// token is scrubbed from every message in the reply.
    fn post(&self, url: &str, token: &str, body: &Value) -> Reply;
}

/// The dry-run view of a call: the input without the token.
pub fn dry_run_input(params: &Value) -> Value {
    let mut copy = params.clone();
    if let Some(map) = copy.as_object_mut() {
        map.remove("token");
    }
    copy
}

fn slug(value: &str, name: &str) -> Result<(), Fail> {
    let ok = !value.is_empty()
        && value.len() <= SLUG_MAX
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(bad(format!(
            "{name} must be a lower-case slug of 1 to {SLUG_MAX} characters from a-z, 0-9, _ and -"
        )))
    }
}

fn check_token(token: &str) -> Result<(), Fail> {
    if token.trim().is_empty() {
        return Err(bad("token must not be empty"));
    }
    if token.chars().count() > MAX_TOKEN_CHARS || token.chars().any(char::is_control) {
        return Err(bad(
            "token must be at most 4096 characters with no control characters",
        ));
    }
    Ok(())
}

fn limit_of(limit: Option<u32>) -> Result<i64, Fail> {
    match limit {
        None => Ok(i64::from(DEFAULT_LIMIT)),
        Some(n) if (1..=MAX_LIMIT).contains(&n) => Ok(i64::from(n)),
        Some(_) => Err(bad("limit must be 1 to 200")),
    }
}

fn outcome_value(event_id: &str, prev: &DmPrev, duplicate: bool) -> Value {
    json!({
        "event_id": event_id,
        "outcome": prev.outcome,
        "reason": prev.reason,
        "rule_id": prev.rule_id,
        "link_id": prev.link_id,
        "remote_id": prev.remote_id,
        "duplicate": duplicate,
        "recorded": true,
    })
}

fn deferred(
    event_id: &str,
    reason: &str,
    detail: Option<String>,
    resets_at: Option<String>,
) -> Value {
    json!({
        "event_id": event_id,
        "outcome": "deferred",
        "reason": reason,
        "detail": detail,
        "resets_at": resets_at,
        "duplicate": false,
        "recorded": false,
    })
}

/// An outcome row with no rule, link, detail or claim.
fn plain_outcome<'a>(
    ev: &'a DmEvent,
    scope: &'a Scope,
    comment_id: &'a str,
    recipient: &'a str,
    decided_at: &'a str,
    outcome: &'a str,
    reason: Option<&'a str>,
) -> NewOutcome<'a> {
    NewOutcome {
        ev,
        scope,
        comment_id,
        recipient,
        outcome,
        reason,
        rule_id: None,
        link_id: None,
        detail: "",
        claimed_at: None,
        decided_at,
    }
}

/// The stored row that blocked an insert: by event first, then by comment and recipient.
fn existing(
    store: &dyn AutomationStore,
    ev: &DmEvent,
    scope: &Scope,
    comment_id: &str,
    recipient: &str,
) -> Result<Value, Fail> {
    if let Some(prev) = store.prev_by_event(&ev.event_id)? {
        return Ok(outcome_value(&ev.event_id, &prev, true));
    }
    match store.prev_by_key(&ev.platform, scope, comment_id, recipient)? {
        Some(prev) => Ok(outcome_value(&ev.event_id, &prev, true)),
        None => Err((
            "internal",
            "an outcome insert was ignored but no row holds the event".to_string(),
        )),
    }
}

/// Route one method of this module. Unknown methods are `unknown_method`. The send commands also need the
/// host's [`Transport`].
pub fn dispatch(
    store: &dyn AutomationStore,
    transport: &dyn Transport,
    method: &str,
    input: Value,
) -> Result<Value, Fail> {
    match method {
        "dm.rule_put" => rule_put(store, input),
        "dm.rule_list" => rule_list(store, input),
        "dm.rule_delete" => rule_delete(store, input),
        "dm.rate_put" => rate_put(store, input),
        "dm.opt_out" => opt_out(store, input),
        "dm.process" => {
            store.require_role()?;
            process(store, transport, input)
        }
        "dm.sweep" => {
            store.require_role()?;
            sweep(store, transport, input)
        }
        "dm.outcome_list" => outcome_list(store, input),
        "dm.draft_list" => draft_list(store, input),
        "dm.draft_dismiss" => draft_dismiss(store, input),
        other => Err(("unknown_method", format!("unknown method: {other}"))),
    }
}

fn platform_ok(platform: &str) -> Result<(), Fail> {
    if DM_PLATFORMS.contains(&platform) {
        Ok(())
    } else {
        Err((
            "platform_unsupported",
            format!("{platform} has no private reply: use instagram or facebook"),
        ))
    }
}

fn scope_json(scope: &Scope) -> Value {
    json!({ "venture": scope.venture, "channel": scope.channel, "account": scope.account })
}

/// `dm.rule_put`: create or replace one keyword rule. A rule with a link cuts (or finds) the tracked link now.
fn rule_put(store: &dyn AutomationStore, input: Value) -> Result<Value, Fail> {
    let req: DmRulePutInput = parse_input(input, "dm.rule_put")?;
    platform_ok(&req.platform)?;
    slug(&req.rule_id, "rule_id")?;
    let keyword = req.keyword.trim().to_string();
    if keyword.chars().count() > MAX_KEYWORD_CHARS || words(&keyword).is_empty() {
        return Err(bad(
            "keyword must have at least one letter or digit and at most 64 characters",
        ));
    }
    let reply = req.reply_text.trim().to_string();
    if reply.is_empty() || reply.chars().count() > MAX_REPLY_CHARS {
        return Err(bad("reply_text must be 1 to 700 characters"));
    }
    if reply.chars().any(|c| c.is_control() && c != '\n') {
        return Err(bad("reply_text must not contain control characters"));
    }
    let now = time::now_secs(req.now.as_deref())?;
    let stamp = time::format_instant(now);
    require_channel(store, &req.scope)?;

    let key_words = words(&keyword);
    for other in store.rules(&req.scope, Some(&req.platform))? {
        if other.rule_id != req.rule_id && words(&other.keyword) == key_words {
            return Err((
                "duplicate_keyword",
                format!(
                    "rule {} already holds this keyword for the account",
                    other.rule_id
                ),
            ));
        }
    }
    let prior = store.rule_created_at(&req.scope, &req.rule_id)?;
    if prior.is_none() && store.rule_count(&req.scope)? >= MAX_RULES_PER_ACCOUNT {
        return Err((
            "too_many_rules",
            format!("an account holds at most {MAX_RULES_PER_ACCOUNT} rules"),
        ));
    }

    let link_id = match &req.link {
        None => None,
        Some(link) => {
            let made = rightkit_social_analytics::links::dispatch(
                store,
                "link.create",
                json!({
                    "scope": scope_json(&req.scope),
                    "campaign_id": link.campaign_id,
                    "variant_id": link.variant_id,
                    "platform": req.platform,
                    "destination": link.destination,
                    "now": stamp,
                }),
            )?;
            let id = made
                .get("link")
                .and_then(|l| l.get("link_id"))
                .and_then(Value::as_str)
                .ok_or(("internal", "link.create returned no link id".to_string()))?;
            Some(id.to_string())
        }
    };
    store.upsert_rule(&NewRule {
        scope: &req.scope,
        rule_id: &req.rule_id,
        platform: &req.platform,
        keyword: &keyword,
        reply_text: &reply,
        link_id: link_id.as_deref(),
        stamp: &stamp,
    })?;
    Ok(json!({
        "created": prior.is_none(),
        "rule": {
            "scope": scope_json(&req.scope),
            "rule_id": req.rule_id,
            "platform": req.platform,
            "keyword": keyword,
            "reply_text": reply,
            "link_id": link_id,
            "created_at": prior.unwrap_or_else(|| stamp.clone()),
            "updated_at": stamp,
        }
    }))
}

fn rule_list(store: &dyn AutomationStore, input: Value) -> Result<Value, Fail> {
    let req: DmRuleListInput = parse_input(input, "dm.rule_list")?;
    require_channel(store, &req.scope)?;
    let rules = store.rules(&req.scope, None)?;
    let items: Vec<Value> = rules
        .iter()
        .map(|r| {
            json!({
                "rule_id": r.rule_id, "platform": r.platform, "keyword": r.keyword, "reply_text": r.reply_text,
                "link_id": r.link_id, "created_at": r.created_at, "updated_at": r.updated_at,
            })
        })
        .collect();
    Ok(json!({ "scope": scope_json(&req.scope), "rules": items }))
}

fn rule_delete(store: &dyn AutomationStore, input: Value) -> Result<Value, Fail> {
    let req: DmRuleDeleteInput = parse_input(input, "dm.rule_delete")?;
    require_channel(store, &req.scope)?;
    if !store.delete_rule(&req.scope, &req.rule_id)? {
        return Err((
            "rule_not_found",
            format!("no rule {} for this account", req.rule_id),
        ));
    }
    Ok(json!({ "deleted": true, "rule_id": req.rule_id }))
}

fn rate_put(store: &dyn AutomationStore, input: Value) -> Result<Value, Fail> {
    let req: DmRatePutInput = parse_input(input, "dm.rate_put")?;
    if req.per_hour == 0 || req.per_hour > 10_000 {
        return Err(bad("per_hour must be 1 to 10000"));
    }
    let stamp = time::format_instant(time::now_secs(req.now.as_deref())?);
    require_channel(store, &req.scope)?;
    store.put_rate(&req.scope, req.per_hour, &stamp)?;
    Ok(json!({ "scope": scope_json(&req.scope), "per_hour": req.per_hour, "updated_at": stamp }))
}

fn per_hour(store: &dyn AutomationStore, scope: &Scope) -> Result<i64, Fail> {
    Ok(store
        .rate(scope)?
        .unwrap_or_else(|| i64::from(DEFAULT_PER_HOUR)))
}

/// Who may record an opt-out: a person, or the daemon relaying the recipient's own request. A skill may not.
fn opt_out_author(by: &str) -> Result<String, Fail> {
    let name = by.trim();
    match name {
        "adrian" | "daemon" => Ok(name.to_string()),
        "skill" => Err((
            "author_forbidden",
            "by is a skill: only adrian or the daemon records an opt-out".to_string(),
        )),
        _ => Err(bad("by must be adrian or daemon")),
    }
}

fn opt_out(store: &dyn AutomationStore, input: Value) -> Result<Value, Fail> {
    let req: DmOptOutInput = parse_input(input, "dm.opt_out")?;
    let by = opt_out_author(&req.by)?;
    let recipient = req.recipient_id.trim().to_string();
    if recipient.is_empty() || recipient.chars().count() > 256 {
        return Err(bad("recipient_id must be 1 to 256 characters"));
    }
    let stamp = time::format_instant(time::now_secs(req.now.as_deref())?);
    require_channel(store, &req.scope)?;
    let added = store.insert_opt_out(&req.scope, &recipient, &by, &stamp)?;
    Ok(json!({ "recipient_id": recipient, "created": added }))
}

struct Ctx<'a> {
    token: &'a str,
    base_url: Option<&'a str>,
    link_base: Option<&'a str>,
    now: i64,
}

/// The public base a tracked link is served from: https, or loopback http for a development daemon.
fn link_base_ok(raw: &str) -> Result<String, Fail> {
    let base = raw.trim().trim_end_matches('/').to_string();
    let ok = base.chars().count() <= 200
        && !base.chars().any(|c| c.is_control() || c.is_whitespace())
        && (base.starts_with("https://")
            || base.starts_with("http://127.0.0.1")
            || base.starts_with("http://localhost"))
        && !base.contains('?')
        && !base.contains('#');
    if ok {
        Ok(base)
    } else {
        Err((
            "link_base_invalid",
            "link_base_url must be an https URL (loopback http in development) with no query"
                .to_string(),
        ))
    }
}

fn process(
    store: &dyn AutomationStore,
    transport: &dyn Transport,
    input: Value,
) -> Result<Value, Fail> {
    let req: DmProcessInput = parse_input(input, "dm.process")?;
    let now = time::now_secs(req.now.as_deref())?;
    let cx = Ctx {
        token: &req.token,
        base_url: req.base_url.as_deref(),
        link_base: req.link_base_url.as_deref(),
        now,
    };
    process_event(store, transport, &req.event_id, &cx)
}

/// What the claim transaction decided.
enum Claim {
    Limited(i64, Option<String>),
    Duplicate,
    Claimed,
}

fn process_event(
    store: &dyn AutomationStore,
    transport: &dyn Transport,
    event_id: &str,
    cx: &Ctx<'_>,
) -> Result<Value, Fail> {
    let Some(ev) = store.dm_event(event_id)? else {
        return Err(("event_not_found", "no inbox event has this id".to_string()));
    };
    if let Some(prev) = store.prev_by_event(&ev.event_id)? {
        return Ok(outcome_value(&ev.event_id, &prev, true));
    }
    let Some(scope) = ev.scope.clone() else {
        return Ok(json!({
            "event_id": ev.event_id, "outcome": "skipped", "reason": "unbound_account",
            "duplicate": false, "recorded": false,
        }));
    };
    let comment_id = ev.comment_id.clone().unwrap_or_else(|| ev.event_id.clone());
    let recipient = ev.sender.clone().unwrap_or_default();
    if let Some(prev) = store.prev_by_key(&ev.platform, &scope, &comment_id, &recipient)? {
        return Ok(outcome_value(&ev.event_id, &prev, true));
    }
    let stamp = time::format_instant(cx.now);

    // Eligibility that needs no rule.
    let skip = if ev.kind != "comment" {
        Some("not_a_comment")
    } else if !DM_PLATFORMS.contains(&ev.platform.as_str()) {
        Some("platform_unsupported")
    } else if ev.state != "open" {
        Some("event_not_open")
    } else if ev.sender.as_deref().map(str::is_empty).unwrap_or(true) {
        Some("no_sender")
    } else if ev.sender.is_some() && ev.sender == ev.recipient {
        Some("own_comment")
    } else if ev
        .text
        .as_deref()
        .map(|t| t.trim().is_empty())
        .unwrap_or(true)
    {
        Some("no_text")
    } else {
        None
    };
    let row = |outcome: &'static str, reason: Option<&'static str>| {
        plain_outcome(
            &ev,
            &scope,
            &comment_id,
            &recipient,
            &stamp,
            outcome,
            reason,
        )
    };
    if let Some(reason) = skip {
        return if store.insert_outcome(&row("skipped", Some(reason)))? {
            Ok(
                json!({ "event_id": ev.event_id, "outcome": "skipped", "reason": reason, "duplicate": false, "recorded": true }),
            )
        } else {
            existing(store, &ev, &scope, &comment_id, &recipient)
        };
    }

    let text = ev.text.clone().unwrap_or_default();
    let rules = store.rules(&scope, Some(&ev.platform))?;
    let hit = rules
        .into_iter()
        .find(|r| keyword_matches(&r.keyword, &text));
    let Some(rule) = hit else {
        // No keyword: queue a draft, send nothing.
        let queued = in_txn(store, || {
            if !store.insert_outcome(&row("drafted", Some("no_keyword")))? {
                return Ok(false);
            }
            store.insert_draft(&NewDraft {
                draft_id: &ev.event_id,
                platform: &ev.platform,
                scope: &scope,
                comment_id: &comment_id,
                sender_id: &recipient,
                comment_text: &text,
                created_at: &stamp,
            })?;
            Ok(true)
        })?;
        if !queued {
            return existing(store, &ev, &scope, &comment_id, &recipient);
        }
        return Ok(json!({
            "event_id": ev.event_id, "outcome": "drafted", "reason": "no_keyword", "draft_id": ev.event_id,
            "duplicate": false, "recorded": true,
        }));
    };

    let refuse = |reason: &str, detail: &str| -> Result<Value, Fail> {
        let out = NewOutcome {
            ev: &ev,
            scope: &scope,
            comment_id: &comment_id,
            recipient: &recipient,
            outcome: "refused",
            reason: Some(reason),
            rule_id: Some(&rule.rule_id),
            link_id: rule.link_id.as_deref(),
            detail,
            claimed_at: None,
            decided_at: &stamp,
        };
        if store.insert_outcome(&out)? {
            Ok(json!({
                "event_id": ev.event_id, "outcome": "refused", "reason": reason, "rule_id": rule.rule_id,
                "duplicate": false, "recorded": true,
            }))
        } else {
            existing(store, &ev, &scope, &comment_id, &recipient)
        }
    };

    // Opt-out, then the window from the event's own time.
    if store.opted_out(&scope, &recipient)? {
        return refuse("opted_out", "");
    }
    let Some(at) = ev.time.as_deref().and_then(event_secs) else {
        return refuse("event_time_unreadable", "");
    };
    let age = cx.now - at;
    if age > DM_WINDOW_SECS {
        return refuse(
            "window_expired",
            &format!("the comment is {age} seconds old; the window is {DM_WINDOW_SECS}"),
        );
    }
    if age < -FUTURE_SKEW_SECS {
        return refuse("event_time_in_future", "");
    }

    // Everything the request needs, checked before the claim so a config error sends nothing and records nothing.
    check_token(cx.token)?;
    let message = match &rule.link_id {
        None => rule.reply_text.clone(),
        Some(link_id) => {
            let Some(raw) = cx.link_base else {
                return Err((
                    "link_base_missing",
                    "the rule has a tracked link: pass link_base_url".to_string(),
                ));
            };
            let base = link_base_ok(raw)?;
            format!(
                "{}\n{}{}{}",
                rule.reply_text,
                base,
                store.public_link_prefix(),
                link_id
            )
        }
    };
    if message.chars().count() > MAX_MESSAGE_CHARS {
        return refuse("text_too_long", "");
    }
    let base = transport.base_url(&ev.platform, cx.base_url)?;
    let account_id = ev.recipient.clone().unwrap_or_default();
    if account_id.is_empty() {
        return refuse("no_account_id", "");
    }
    let version = rightkit_social_publish::api_versions::pinned("facebook")
        .map(|p| p.version)
        .unwrap_or("v25.0");
    let url = format!(
        "{base}/{}/{}/{version}/{}/messages",
        percent_encode(&ev.platform),
        percent_encode(&account_id),
        percent_encode(&account_id)
    );

    // Claim, under the rate limit, in one transaction.
    let limit = per_hour(store, &scope)?;
    let claim = in_txn(store, || {
        let since = time::format_instant(cx.now - 3600);
        let (used, oldest) = store.claims_since(&scope, &since)?;
        if used >= limit {
            return Ok(Claim::Limited(used, oldest));
        }
        let out = NewOutcome {
            ev: &ev,
            scope: &scope,
            comment_id: &comment_id,
            recipient: &recipient,
            outcome: "sending",
            reason: None,
            rule_id: Some(&rule.rule_id),
            link_id: rule.link_id.as_deref(),
            detail: "",
            claimed_at: Some(&stamp),
            decided_at: &stamp,
        };
        if !store.insert_outcome(&out)? {
            return Ok(Claim::Duplicate);
        }
        Ok(Claim::Claimed)
    })?;
    match claim {
        Claim::Limited(used, oldest) => {
            let resets = oldest
                .as_deref()
                .and_then(|o| time::parse_instant(o, "claimed_at").ok())
                .map(|s| time::format_instant(s + 3600));
            return Ok(deferred(
                &ev.event_id,
                "rate_limited",
                Some(format!("{used} of {limit} private replies this hour")),
                resets,
            ));
        }
        Claim::Duplicate => return existing(store, &ev, &scope, &comment_id, &recipient),
        Claim::Claimed => {}
    }

    // The one platform call. The claim is committed: whatever happens next, this event is never sent twice.
    let body = json!({ "recipient": { "comment_id": comment_id }, "message": { "text": message } });
    let reply = transport.post(&url, cx.token, &body);
    let release = |reason: &str, detail: Option<String>| -> Result<Value, Fail> {
        store.delete_claim(&ev.event_id)?;
        Ok(deferred(&ev.event_id, reason, detail, None))
    };
    let finish = |outcome: &str,
                  reason: Option<&str>,
                  remote: Option<String>,
                  detail: &str|
     -> Result<Value, Fail> {
        store.finish_claim(&Finish {
            event_id: &ev.event_id,
            outcome,
            reason,
            remote_id: remote.as_deref(),
            detail,
            decided_at: &stamp,
        })?;
        let prev = store
            .prev_by_event(&ev.event_id)?
            .ok_or(("internal", "the claimed outcome row is missing".to_string()))?;
        Ok(outcome_value(&ev.event_id, &prev, false))
    };
    match reply {
        Reply::Ok(answer) => {
            let remote = answer
                .get("message_id")
                .and_then(Value::as_str)
                .map(|s| s.chars().take(256).collect::<String>());
            finish("sent", None, remote, "")
        }
        Reply::RateLimited(_) => release("platform_rate_limited", None),
        Reply::Unauthorized => release("token_rejected", None),
        Reply::Transport {
            connect_phase: true,
            message,
        } => release("transport_unreachable", Some(message)),
        Reply::Invalid(message) => release("request_invalid", Some(message)),
        Reply::Rejected { status, message } => finish(
            "rejected",
            Some("platform_rejected"),
            None,
            &format!("{status}: {message}"),
        ),
        Reply::ServerError(status) => finish(
            "unknown",
            Some("send_unconfirmed"),
            None,
            &format!("server error {status}"),
        ),
        Reply::Transport {
            connect_phase: false,
            message,
        } => finish("unknown", Some("send_unconfirmed"), None, &message),
        Reply::Timeout => finish("unknown", Some("send_unconfirmed"), None, "timeout"),
    }
}

/// Reasons that make the rest of a sweep pointless: the platform or the token, not one event.
fn stops_sweep(reason: &str) -> bool {
    matches!(
        reason,
        "platform_rate_limited" | "token_rejected" | "transport_unreachable" | "request_invalid"
    )
}

fn sweep(
    store: &dyn AutomationStore,
    transport: &dyn Transport,
    input: Value,
) -> Result<Value, Fail> {
    let req: DmSweepInput = parse_input(input, "dm.sweep")?;
    check_token(&req.token)?;
    let now = time::now_secs(req.now.as_deref())?;
    let limit = limit_of(req.limit)?;
    require_channel(store, &req.scope)?;
    let ids = store.unprocessed_comment_ids(&req.scope, limit)?;
    let cx = Ctx {
        token: &req.token,
        base_url: req.base_url.as_deref(),
        link_base: req.link_base_url.as_deref(),
        now,
    };
    let mut results: Vec<Value> = Vec::new();
    let mut stopped: Option<String> = None;
    for id in &ids {
        let result = process_event(store, transport, id, &cx)?;
        let outcome = result
            .get("outcome")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let reason = result
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        results.push(result);
        if outcome == "deferred" && stops_sweep(&reason) {
            stopped = Some(reason);
            break;
        }
    }
    let count = |name: &str| {
        results
            .iter()
            .filter(|r| r.get("outcome").and_then(Value::as_str) == Some(name))
            .count()
    };
    Ok(json!({
        "scope": scope_json(&req.scope),
        "considered": ids.len(),
        "sent": count("sent"),
        "drafted": count("drafted"),
        "refused": count("refused"),
        "skipped": count("skipped"),
        "rejected": count("rejected"),
        "unknown": count("unknown"),
        "deferred": count("deferred"),
        "stopped": stopped,
        "results": results,
    }))
}

fn outcome_list(store: &dyn AutomationStore, input: Value) -> Result<Value, Fail> {
    let req: DmOutcomeListInput = parse_input(input, "dm.outcome_list")?;
    let limit = limit_of(req.limit)?;
    require_channel(store, &req.scope)?;
    let items = store.outcome_rows(&req.scope, req.outcome.as_deref(), limit)?;
    Ok(json!({ "scope": scope_json(&req.scope), "outcomes": items }))
}

fn draft_list(store: &dyn AutomationStore, input: Value) -> Result<Value, Fail> {
    let req: DmDraftListInput = parse_input(input, "dm.draft_list")?;
    let limit = limit_of(req.limit)?;
    if let Some(state) = req.state.as_deref() {
        if !matches!(state, "pending" | "dismissed") {
            return Err(bad("state must be pending or dismissed"));
        }
    }
    require_channel(store, &req.scope)?;
    let items = store.draft_rows(&req.scope, req.state.as_deref(), limit)?;
    Ok(json!({ "scope": scope_json(&req.scope), "drafts": items }))
}

fn draft_dismiss(store: &dyn AutomationStore, input: Value) -> Result<Value, Fail> {
    let req: DmDraftDismissInput = parse_input(input, "dm.draft_dismiss")?;
    let by = require_human(&req.by)?;
    let stamp = time::format_instant(time::now_secs(req.now.as_deref())?);
    require_channel(store, &req.scope)?;
    if !store.dismiss_draft(&req.scope, &req.draft_id, &by, &stamp)? {
        return Err((
            "draft_not_found",
            "no draft with this id for this account".to_string(),
        ));
    }
    Ok(json!({ "draft_id": req.draft_id, "state": "dismissed", "state_by": by, "state_at": stamp }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opt_out_is_for_a_person_or_the_daemon() {
        assert_eq!(opt_out_author(" adrian ").expect("a person"), "adrian");
        assert_eq!(opt_out_author("daemon").expect("the daemon"), "daemon");
        assert!(matches!(opt_out_author("skill"), Err((c, _)) if c == "author_forbidden"));
        assert!(matches!(opt_out_author("x"), Err((c, _)) if c == "bad_input"));
    }

    #[test]
    fn link_bases_are_https_or_loopback_without_a_query() {
        assert_eq!(
            link_base_ok(" https://a.example/ ").expect("https"),
            "https://a.example"
        );
        assert!(link_base_ok("http://127.0.0.1:9").is_ok());
        for bad_base in ["http://a.example", "https://a.example/?q", "https://a b"] {
            assert!(link_base_ok(bad_base).is_err(), "{bad_base}");
        }
    }

    #[test]
    fn the_token_is_checked_before_a_send() {
        assert!(check_token("t").is_ok());
        assert!(check_token("  ").is_err());
        assert!(check_token("a\nb").is_err());
    }

    #[test]
    fn dry_run_drops_the_token() {
        let shown = dry_run_input(&json!({ "token": "x", "event_id": "e" }));
        assert_eq!(shown, json!({ "event_id": "e" }));
    }
}
