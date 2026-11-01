//! Payload shapes for tracked links (desktop store): campaigns, the links cut from them, and the visits
//! recorded against those links. A link is keyed by venture, channel, account and a link id derived from
//! the scope, campaign, variant, platform and destination, so the same inputs always name the same link.
//! Nothing here fetches a URL or redirects a visitor: the engine builds the final URL with its UTM
//! parameters and stores visits that the operator or a later import reports.
use serde::{Deserialize, Serialize};

use rightkit_social_publish::protocol::Scope;

pub use crate::links::UtmPolicy;

/// One campaign. `utm_campaign` is the slug written into `utm_campaign` on every link cut after it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Campaign {
    pub scope: Scope,
    pub campaign_id: String,
    pub name: String,
    pub utm_campaign: String,
    /// Compare-and-swap revision. Every write bumps it.
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// `campaign.put` input. Creates the campaign when it does not exist (`expected_revision` absent or 0);
/// otherwise `expected_revision` must equal the stored revision. `utm_campaign` defaults to the campaign
/// id on create and keeps the stored value on edit when absent. A changed `utm_campaign` affects only
/// links cut afterwards: a stored link keeps the final URL it was created with.
#[derive(Debug, Clone, Deserialize)]
pub struct CampaignPutInput {
    pub scope: Scope,
    pub campaign_id: String,
    pub name: String,
    #[serde(default)]
    pub utm_campaign: Option<String>,
    #[serde(default)]
    pub expected_revision: Option<i64>,
    #[serde(default)]
    pub now: Option<String>,
}

/// `campaign.list` input: every campaign in one scope.
#[derive(Debug, Clone, Deserialize)]
pub struct CampaignListInput {
    pub scope: Scope,
}

/// `campaign.list` output, ordered by campaign id ascending.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CampaignListOutput {
    pub scope: Scope,
    pub campaigns: Vec<Campaign>,
}

/// A link named by the inputs it was derived from, instead of by its id.
#[derive(Debug, Clone, Deserialize)]
pub struct LinkKey {
    pub campaign_id: String,
    pub variant_id: String,
    pub platform: String,
    pub destination: String,
}

/// One tracked link, as every link command returns it. `final_url` is the destination with the UTM
/// parameters appended; it is fixed when the link is created.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedLink {
    pub scope: Scope,
    pub link_id: String,
    pub campaign_id: String,
    pub variant_id: String,
    pub platform: String,
    /// The https destination as given, trimmed.
    pub destination: String,
    /// The calendar entry the link was cut for. Null when it was not attached to one.
    pub calendar_entry_id: Option<String>,
    pub utm_source: String,
    pub utm_medium: String,
    pub utm_campaign: String,
    pub utm_content: String,
    pub final_url: String,
    pub created_at: String,
}

/// `link.create` input. Idempotent: the same scope, campaign, variant, platform and destination return the
/// stored link, with `created` false, when the calendar entry and final URL match; otherwise `link_conflict`.
#[derive(Debug, Clone, Deserialize)]
pub struct LinkCreateInput {
    pub scope: Scope,
    pub campaign_id: String,
    pub variant_id: String,
    pub platform: String,
    pub destination: String,
    #[serde(default)]
    pub calendar_entry_id: Option<String>,
    #[serde(default)]
    pub utm: Option<UtmPolicy>,
    #[serde(default)]
    pub now: Option<String>,
}

/// `link.create` output. `created` is false when the same link already existed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkCreateOutput {
    pub link: TrackedLink,
    pub created: bool,
}

/// `link.get` input. Exactly one of `link_id` or `link` names the link.
#[derive(Debug, Clone, Deserialize)]
pub struct LinkGetInput {
    pub scope: Scope,
    #[serde(default)]
    pub link_id: Option<String>,
    #[serde(default)]
    pub link: Option<LinkKey>,
}

/// `link.list` input. At least one of `campaign_id` or `calendar_entry_id` is required; both filter.
#[derive(Debug, Clone, Deserialize)]
pub struct LinkListInput {
    pub scope: Scope,
    #[serde(default)]
    pub campaign_id: Option<String>,
    #[serde(default)]
    pub calendar_entry_id: Option<String>,
}

/// `link.list` output, ordered by campaign id, variant id, platform and link id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkListOutput {
    pub scope: Scope,
    pub links: Vec<TrackedLink>,
}

/// `link.record_visit` input. `visit_id` is required and is the idempotency key. Exactly one of `link_id`
/// or `link` names the link. `visited_at` defaults to `now`.
#[derive(Debug, Clone, Deserialize)]
pub struct LinkRecordVisitInput {
    pub scope: Scope,
    pub visit_id: String,
    #[serde(default)]
    pub link_id: Option<String>,
    #[serde(default)]
    pub link: Option<LinkKey>,
    #[serde(default)]
    pub visited_at: Option<String>,
    #[serde(default)]
    pub source_note: String,
    #[serde(default)]
    pub now: Option<String>,
}

/// One recorded visit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkVisit {
    pub scope: Scope,
    pub visit_id: String,
    pub link_id: String,
    /// UTC instant, `YYYY-MM-DDTHH:MM:SSZ`.
    pub visited_at: String,
    pub source_note: String,
}

/// `link.record_visit` output. `recorded` is false when the visit id was already stored; the stored visit
/// is returned unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkRecordVisitOutput {
    pub visit: LinkVisit,
    pub recorded: bool,
}

/// `link.attribution` input: one campaign in one scope.
#[derive(Debug, Clone, Deserialize)]
pub struct LinkAttributionInput {
    pub scope: Scope,
    pub campaign_id: String,
}

/// One link's plain visit count, joined to the variant, account, platform and calendar entry that cut it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributionRow {
    pub link_id: String,
    pub variant_id: String,
    pub platform: String,
    pub account: String,
    pub calendar_entry_id: Option<String>,
    pub visits: u64,
}

/// `link.attribution` output. Counts only: no rates and no ranking. Ordered by variant id, platform and
/// link id. Links with no visits are included with a count of zero.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkAttributionOutput {
    pub scope: Scope,
    pub campaign_id: String,
    pub links: Vec<AttributionRow>,
    pub total_visits: u64,
}
