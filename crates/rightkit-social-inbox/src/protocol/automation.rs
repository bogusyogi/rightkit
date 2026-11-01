//! Payload shapes for DM automation: keyword comment to private reply, per account.
//! A rule belongs to one account of one channel. The engine sends a private reply only from an inbox comment
//! event that matches a rule, once per comment and recipient, inside the 24 hour messaging window counted from the
//! event's own time, under a per-account hourly limit. A comment that matches no rule is queued as a draft for the
//! operator; nothing is sent for it. Event ids, comment ids, recipient ids and text come from a verified payload and
//! are untrusted data: stored as given, never put in a path or logged.
//!
//! The access token is never a field of a stored shape. The daemon supplies `token` in the call input; the engine
//! uses it for one platform request and never stores, returns or logs it.
use serde::Deserialize;

use rightkit_social_publish::protocol::Scope;

/// Platforms whose comments can be answered with a private reply.
pub const DM_PLATFORMS: [&str; 2] = ["instagram", "facebook"];

/// The messaging window, in seconds, counted from the comment event's own time. Instagram Login documents 24 hours;
/// the Facebook Login route is unverified, so the stricter 24 hours applies to both.
pub const DM_WINDOW_SECS: i64 = 86_400;

/// Final outcomes recorded for an inbox comment event. `deferred` is never recorded: the event stays unprocessed.
pub const DM_OUTCOMES: [&str; 7] = [
    "sending", "sent", "unknown", "rejected", "refused", "drafted", "skipped",
];

/// The link a rule attaches to its reply. Created (idempotently) when the rule is stored, so a bad campaign
/// fails then and not at send time.
#[derive(Debug, Clone, Deserialize)]
pub struct DmRuleLink {
    pub campaign_id: String,
    pub variant_id: String,
    pub destination: String,
}

/// `dm.rule_put` input: create or replace one keyword rule.
#[derive(Debug, Clone, Deserialize)]
pub struct DmRulePutInput {
    pub scope: Scope,
    pub platform: String,
    pub rule_id: String,
    pub keyword: String,
    pub reply_text: String,
    #[serde(default)]
    pub link: Option<DmRuleLink>,
    #[serde(default)]
    pub now: Option<String>,
}

/// `dm.rule_list` input.
#[derive(Debug, Clone, Deserialize)]
pub struct DmRuleListInput {
    pub scope: Scope,
}

/// `dm.rule_delete` input.
#[derive(Debug, Clone, Deserialize)]
pub struct DmRuleDeleteInput {
    pub scope: Scope,
    pub rule_id: String,
}

/// `dm.rate_put` input: the most private replies one account may send in any rolling hour.
#[derive(Debug, Clone, Deserialize)]
pub struct DmRatePutInput {
    pub scope: Scope,
    pub per_hour: u32,
    #[serde(default)]
    pub now: Option<String>,
}

/// `dm.opt_out` input: a recipient who must receive no further reply from this account.
#[derive(Debug, Clone, Deserialize)]
pub struct DmOptOutInput {
    pub scope: Scope,
    pub recipient_id: String,
    pub by: String,
    #[serde(default)]
    pub now: Option<String>,
}

/// `dm.process` input: one inbox event. `token` is the account's access token.
#[derive(Debug, Clone, Deserialize)]
pub struct DmProcessInput {
    pub event_id: String,
    pub token: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub link_base_url: Option<String>,
    #[serde(default)]
    pub now: Option<String>,
}

/// `dm.sweep` input: every unprocessed open comment event of one account, oldest arrival first.
#[derive(Debug, Clone, Deserialize)]
pub struct DmSweepInput {
    pub scope: Scope,
    pub token: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub link_base_url: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub now: Option<String>,
}

/// `dm.outcome_list` input.
#[derive(Debug, Clone, Deserialize)]
pub struct DmOutcomeListInput {
    pub scope: Scope,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// `dm.draft_list` input. `state` is `pending` or `dismissed`.
#[derive(Debug, Clone, Deserialize)]
pub struct DmDraftListInput {
    pub scope: Scope,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// `dm.draft_dismiss` input: a person closes a queued draft. Sends nothing.
#[derive(Debug, Clone, Deserialize)]
pub struct DmDraftDismissInput {
    pub scope: Scope,
    pub draft_id: String,
    pub by: String,
    #[serde(default)]
    pub now: Option<String>,
}
