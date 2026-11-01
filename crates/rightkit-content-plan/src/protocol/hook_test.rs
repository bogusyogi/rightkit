//! Payload shapes for the hook test module (channel store schema v15). A variant set is one brief body with
//! N openings (2 to 5). Each opening is bound to a published publish job. `hook_test.read` compares the
//! openings from stored analytics observations and decides a winner: the winner is `promoted`, the others
//! are `archived`, and each keeps its value, observation instant and pull id as lineage.
use serde::{Deserialize, Serialize};

use rightkit_social_publish::protocol::Scope;

/// Set states. A `decided` set never changes.
pub const HOOK_TEST_STATES: &[&str] = &["open", "decided"];

/// `hook_test.create` input. `openings` holds the opening texts in order (2 to 5). `metric` is compared over
/// `window` (`lifetime`, the only window stored). `now` is the creation instant.
#[derive(Debug, Clone, Deserialize)]
pub struct HookTestCreateInput {
    pub scope: Scope,
    pub set_id: String,
    /// Free text naming the brief the body belongs to.
    pub brief_ref: String,
    pub body: String,
    pub openings: Vec<String>,
    pub metric: String,
    #[serde(default)]
    pub window: Option<String>,
    /// UTC instant `YYYY-MM-DDTHH:MM:SSZ`.
    pub now: String,
}

/// `hook_test.bind` input. `opening_index` is 1-based. `expected_revision` must equal the set's revision.
#[derive(Debug, Clone, Deserialize)]
pub struct HookTestBindInput {
    pub scope: Scope,
    pub set_id: String,
    pub opening_index: i64,
    /// A publish job in the same scope that is in state `published`.
    pub job_id: String,
    pub expected_revision: i64,
    /// UTC instant `YYYY-MM-DDTHH:MM:SSZ`, written as the bind time.
    pub now: String,
}

/// `hook_test.read` input. `now` is the evaluation instant: only observations taken at or before it count.
/// `expected_revision` is checked only when this read would decide the set.
#[derive(Debug, Clone, Deserialize)]
pub struct HookTestReadInput {
    pub scope: Scope,
    pub set_id: String,
    #[serde(default)]
    pub expected_revision: Option<i64>,
    /// UTC instant `YYYY-MM-DDTHH:MM:SSZ`.
    pub now: String,
}

/// `hook_test.get` input.
#[derive(Debug, Clone, Deserialize)]
pub struct HookTestGetInput {
    pub scope: Scope,
    pub set_id: String,
}

/// `hook_test.list` input. `state` filters when given.
#[derive(Debug, Clone, Deserialize)]
pub struct HookTestListInput {
    pub scope: Scope,
    #[serde(default)]
    pub state: Option<String>,
}

/// One opening of a set. Binding fills `job_id` and `bound_at`. Deciding fills `role`, and for every opening
/// (winner or not) `value`, `observed_at`, `pull_id` and `published_at`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookTestOpening {
    /// 1-based position of the opening in the body.
    pub opening_index: i64,
    pub text: String,
    pub job_id: Option<String>,
    pub bound_at: Option<String>,
    /// `promoted` for the winner, `archived` for the others, null while the set is open.
    pub role: Option<String>,
    /// The metric value, an exact integer string, from the observation compared.
    pub value: Option<String>,
    /// The instant of the observation compared (the pull's `observed_at`).
    pub observed_at: Option<String>,
    /// The analytics pull that stored the observation compared.
    pub pull_id: Option<String>,
    /// The job's publish instant the 24-hour window counted from.
    pub published_at: Option<String>,
}

/// A variant set with its openings in index order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookTestSet {
    pub scope: Scope,
    pub set_id: String,
    pub brief_ref: String,
    pub body: String,
    pub metric: String,
    pub window: String,
    /// `open` or `decided`.
    pub state: String,
    /// Compare-and-swap revision. Every bind and the decision bump it.
    pub revision: i64,
    /// The 1-based index of the promoted opening once decided, otherwise null.
    pub winner_opening: Option<i64>,
    pub decided_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub openings: Vec<HookTestOpening>,
}

/// `hook_test.read` output. `status` is `decided` (the stored winner, whether this call decided it or an
/// earlier one did) or `insufficient_evidence` (nothing stored; `reason` names why, `message` the figures).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookTestReadOutput {
    pub scope: Scope,
    pub set_id: String,
    pub status: String,
    pub reason: Option<String>,
    pub message: Option<String>,
    pub set: HookTestSet,
}

/// `hook_test.list` output, ordered by creation then set id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookTestListOutput {
    pub scope: Scope,
    pub sets: Vec<HookTestSet>,
}
