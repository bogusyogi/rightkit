//! Payload shapes for connected accounts and their readiness. An account is keyed by
//! venture, channel, account and platform: a channel scope holds one account per platform, as the
//! fixture `accounts` map does. Every state only the daemon can know is stored as `unknown` until a
//! writer reports it; readiness is derived from the stored fields and never guessed.
use serde::{Deserialize, Serialize};

use super::Scope;

/// Connection states. `unknown` is the value until a writer reports the connection.
pub const CONNECTION_STATES: [&str; 4] =
    ["not_connected", "connected", "reauth_required", "unknown"];

/// States of each entitlement. The three entitlement fields are independent: none implies another.
pub const ENTITLEMENT_STATES: [&str; 5] =
    ["not_started", "in_progress", "passed", "blocked", "unknown"];

/// Login routes for an Instagram account. Null on every other platform.
pub const INSTAGRAM_LOGIN_ROUTES: [&str; 2] = ["instagram_login", "facebook_login"];

/// The product's platform vocabulary. `account.put` and `account.import` refuse any other platform
/// name with `bad_input`, and the message lists these values.
pub const PLATFORMS: [&str; 10] = [
    "instagram",
    "facebook",
    "threads",
    "youtube",
    "tiktok",
    "x",
    "linkedin",
    "pinterest",
    "bluesky",
    "mastodon",
];

/// Readiness derived from the stored fields at a given `now`. `can_publish` is true exactly when
/// `blockers` is empty. Blocker codes: `not_connected`, `reauth_required`, `paused`, `cooldown`,
/// `mechanics_not_passed`, `connection_unknown`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountReadiness {
    pub can_publish: bool,
    pub blockers: Vec<String>,
}

/// One stored account, as every account command returns it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountRecord {
    pub scope: Scope,
    pub platform: String,
    pub handle: String,
    pub display_name: String,
    pub account_kind: String,
    /// Set only for `instagram`: `instagram_login` or `facebook_login`. Null elsewhere and until known.
    pub login_route: Option<String>,
    pub connection_state: String,
    /// UTC instant, `YYYY-MM-DDTHH:MM:SSZ`, or null when the expiry is not known.
    pub token_expires_at: Option<String>,
    pub sandbox_mechanics: String,
    pub review_submission: String,
    pub audience_visible: String,
    /// UTC instant the quota cooldown ends. Active while it is after `now`.
    pub quota_cooldown_until: Option<String>,
    pub paused: bool,
    pub paused_reason: Option<String>,
    /// UTC instant of the pause, set while paused.
    pub paused_at: Option<String>,
    pub revision: i64,
    pub updated_at: String,
    pub readiness: AccountReadiness,
}

/// `account.put` input. Creates the account when it does not exist (`expected_revision` absent or 0);
/// otherwise `expected_revision` must equal the stored revision. Pause state is not set here.
#[derive(Debug, Clone, Deserialize)]
pub struct AccountPutInput {
    pub scope: Scope,
    pub platform: String,
    pub handle: String,
    #[serde(default)]
    pub display_name: String,
    pub account_kind: String,
    #[serde(default)]
    pub login_route: Option<String>,
    pub connection_state: String,
    #[serde(default)]
    pub token_expires_at: Option<String>,
    pub sandbox_mechanics: String,
    pub review_submission: String,
    pub audience_visible: String,
    #[serde(default)]
    pub quota_cooldown_until: Option<String>,
    #[serde(default)]
    pub expected_revision: Option<i64>,
    #[serde(default)]
    pub now: Option<String>,
}

/// `account.import` input: a fixture-shaped channel directory whose `scope.json` has an `accounts` map.
#[derive(Debug, Clone, Deserialize)]
pub struct AccountImportInput {
    pub dir: String,
    #[serde(default)]
    pub now: Option<String>,
}

/// `account.get` input.
#[derive(Debug, Clone, Deserialize)]
pub struct AccountGetInput {
    pub scope: Scope,
    pub platform: String,
    #[serde(default)]
    pub now: Option<String>,
}

/// `account.list` input. Without `scope`, every stored account is listed.
#[derive(Debug, Clone, Deserialize)]
pub struct AccountListInput {
    #[serde(default)]
    pub scope: Option<Scope>,
    #[serde(default)]
    pub now: Option<String>,
}

/// `account.pause` input. `reason` is required. `expected_revision` must match the stored revision.
#[derive(Debug, Clone, Deserialize)]
pub struct AccountPauseInput {
    pub scope: Scope,
    pub platform: String,
    pub reason: String,
    pub expected_revision: i64,
    #[serde(default)]
    pub now: Option<String>,
}

/// `account.resume` input. `expected_revision` must match the stored revision.
#[derive(Debug, Clone, Deserialize)]
pub struct AccountResumeInput {
    pub scope: Scope,
    pub platform: String,
    pub expected_revision: i64,
    #[serde(default)]
    pub now: Option<String>,
}

/// `account.list` output, ordered by venture, platform, handle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountListOutput {
    pub accounts: Vec<AccountRecord>,
}

/// `account.import` output. `created` counts new rows; `existing` counts rows already stored, which
/// keep their state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountImportOutput {
    pub scope: Scope,
    pub created: u64,
    pub existing: u64,
    pub accounts: Vec<AccountRecord>,
}

/// `account.disconnect` input. `expected_revision` must match the stored revision.
#[derive(Debug, Clone, Deserialize)]
pub struct AccountDisconnectInput {
    pub scope: Scope,
    pub platform: String,
    pub expected_revision: i64,
    #[serde(default)]
    pub now: Option<String>,
}

/// `account.disconnect` output. `cancelled_jobs` are the publish job ids the call cancelled (queued or waiting, with
/// nothing in flight and no remote post). `kept_jobs` are job ids that may already have a post on the platform
/// (running, or holding a remote id): the call leaves them for the daemon to reconcile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountDisconnectOutput {
    pub account: AccountRecord,
    pub cancelled_jobs: Vec<String>,
    pub kept_jobs: Vec<String>,
}
