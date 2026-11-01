//! Payload shapes for the optimise module. `optimise.evaluate` reads stored
//! analytics observations and stores calendar change proposals. The one rule in this slice is `remake`:
//! a published post in a channel's top decile by a named metric and window yields a remake proposal.
//! Proposals are listed, read, accepted or dismissed. Accepting a remake creates one draft calendar entry
//! (through the calendar commands, not its tables) and records its id on the proposal.
use serde::{Deserialize, Serialize};

use rightkit_social_publish::protocol::Scope;

/// Proposal states. Only `proposed` can be accepted or dismissed.
pub const PROPOSAL_STATES: &[&str] = &["proposed", "accepted", "dismissed"];

/// `optimise.evaluate` input. `now` is the instant of the evaluation and sets the remake due date. The
/// metric is compared over the window (`lifetime`, the only window stored). `min_sample` defaults to 10.
#[derive(Debug, Clone, Deserialize)]
pub struct OptimiseEvaluateInput {
    pub scope: Scope,
    pub metric: String,
    #[serde(default)]
    pub window: Option<String>,
    #[serde(default)]
    pub min_sample: Option<i64>,
    /// UTC instant `YYYY-MM-DDTHH:MM:SSZ`.
    pub now: String,
}

/// One calendar change proposal. `lineage_job_ids` are the published posts compared, and
/// `lineage_pull_ids` are the analytics pulls whose observations were compared.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimiseProposal {
    pub scope: Scope,
    pub proposal_id: String,
    /// The rule that produced it. Only `remake` in this slice.
    pub kind: String,
    /// `proposed`, `accepted` or `dismissed`.
    pub state: String,
    /// Compare-and-swap revision. Every decision bumps it.
    pub revision: i64,
    /// The published post the proposal is about. A remake is proposed at most once per source job.
    pub source_job_id: String,
    pub platform: String,
    pub metric: String,
    pub window: String,
    /// The observation instant whose values were compared.
    pub observed_at: String,
    /// The source post's value of the metric, an exact integer string.
    pub value: String,
    /// The value at the top-decile cut (the smallest value in the decile).
    pub threshold: String,
    pub sample_size: i64,
    pub decile_size: i64,
    /// For a remake: `now` plus 14 days, UTC.
    pub due_at: String,
    pub lineage_job_ids: Vec<String>,
    pub lineage_pull_ids: Vec<String>,
    pub decision_note: Option<String>,
    pub decided_at: Option<String>,
    /// For an accepted remake: the calendar entry it created (`remake_entry_` plus the proposal id). Null
    /// for a proposal not accepted, and for one dismissed.
    pub calendar_entry_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// `optimise.evaluate` output. `status` is `evaluated` (proposals may be listed, and `created` counts the
/// new ones) or `insufficient_evidence` (no proposal, `reason` names why, `message` says the figures).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimiseEvaluateOutput {
    pub scope: Scope,
    pub rule: String,
    pub metric: String,
    pub window: String,
    pub min_sample: i64,
    pub status: String,
    pub reason: Option<String>,
    pub message: Option<String>,
    pub observed_at: Option<String>,
    pub sample_size: i64,
    pub decile_size: i64,
    pub threshold: Option<String>,
    /// Proposals created by this call. Zero on a replay.
    pub created: i64,
    /// Every proposal this evaluation selected, created now or stored earlier.
    pub proposals: Vec<OptimiseProposal>,
}

/// `optimise.list` input. `state` filters when given.
#[derive(Debug, Clone, Deserialize)]
pub struct OptimiseListInput {
    pub scope: Scope,
    #[serde(default)]
    pub state: Option<String>,
}

/// `optimise.list` output, ordered by creation then proposal id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimiseListOutput {
    pub scope: Scope,
    pub proposals: Vec<OptimiseProposal>,
}

/// `optimise.get` input.
#[derive(Debug, Clone, Deserialize)]
pub struct OptimiseGetInput {
    pub scope: Scope,
    pub proposal_id: String,
}

/// `optimise.accept` and `optimise.dismiss` input. `expected_revision` must equal the stored revision.
#[derive(Debug, Clone, Deserialize)]
pub struct OptimiseDecideInput {
    pub scope: Scope,
    pub proposal_id: String,
    pub expected_revision: i64,
    #[serde(default)]
    pub note: Option<String>,
    /// UTC instant `YYYY-MM-DDTHH:MM:SSZ`, written as the decision time.
    pub now: String,
}
