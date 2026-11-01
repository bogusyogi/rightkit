//! Payload shapes for typed briefs and platform-native variants (desktop store). A typed brief is one of
//! seven brief types, each with its own required fields, stored in venture, channel and account scope. A
//! variant is one platform-native rendering of one stored brief at one brief revision. The per-platform rules
//! (caption limit, hashtag limit, link placement, duration limit) live only in the rightkit-platform-rules
//! catalog; a variant records the catalog revision id it was derived under. Nothing here names a platform
//! limit, calls a platform or fetches a URL.
use serde::{Deserialize, Serialize};

use rightkit_social_publish::protocol::Scope;

/// The brief types the store accepts.
pub const BRIEF_TYPES: [&str; 7] = [
    "talking_head",
    "faceless_cartoon_short",
    "explainer",
    "carousel",
    "ugc_character_review",
    "product_pitch",
    "product_demo",
];

/// Where a rule places a link. `none` means the platform carries no link.
pub const LINK_PLACEMENTS: [&str; 4] = ["caption", "bio", "first_comment", "none"];

/// The five beats every explainer needs, in story order.
pub const EXPLAINER_BEATS: [&str; 5] = ["question", "model", "proof", "turn", "payoff"];

/// One scene of an explainer beat. Every field is required and non-empty.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SceneSpec {
    /// What the scene teaches the viewer.
    pub teaches: String,
    /// What the frame shows.
    pub frame: String,
    /// How the frame moves.
    pub motion: String,
    /// The words spoken or shown.
    pub words: String,
    /// The sound under the scene.
    pub sound: String,
}

/// `typed_brief.put` form of the explainer beats. Each beat may be absent so that the error names it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ExplainerBeatsInput {
    #[serde(default)]
    pub question: Option<SceneSpec>,
    #[serde(default)]
    pub model: Option<SceneSpec>,
    #[serde(default)]
    pub proof: Option<SceneSpec>,
    #[serde(default)]
    pub turn: Option<SceneSpec>,
    #[serde(default)]
    pub payoff: Option<SceneSpec>,
}

/// The five beats of a stored explainer, each with its scene spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplainerBeats {
    pub question: SceneSpec,
    pub model: SceneSpec,
    pub proof: SceneSpec,
    pub turn: SceneSpec,
    pub payoff: SceneSpec,
}

/// One product-demo step: the text said while the step is recorded. Non-empty.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DemoStep {
    pub say: String,
}

/// The type-specific part of a brief, as stored. Explainer beats belong only to explainers and demo steps
/// only to product demos.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BriefDetail {
    #[serde(default)]
    pub explainer: Option<ExplainerBeats>,
    #[serde(default)]
    pub demo_steps: Vec<DemoStep>,
}

/// `typed_brief.put` input. Creates the brief when it does not exist (`expected_revision` absent or 0);
/// otherwise `expected_revision` must equal the stored revision.
#[derive(Debug, Clone, Deserialize)]
pub struct TypedBriefPutInput {
    pub scope: Scope,
    pub brief_id: String,
    pub brief_type: String,
    pub hook: String,
    pub body: String,
    #[serde(default)]
    pub explainer: Option<ExplainerBeatsInput>,
    #[serde(default)]
    pub demo_steps: Vec<DemoStep>,
    #[serde(default)]
    pub expected_revision: Option<i64>,
    #[serde(default)]
    pub now: Option<String>,
}

/// One stored typed brief, as every brief command returns it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypedBrief {
    pub scope: Scope,
    pub brief_id: String,
    pub brief_type: String,
    pub hook: String,
    pub body: String,
    /// Present exactly when `brief_type` is `explainer`.
    pub explainer: Option<ExplainerBeats>,
    /// Non-empty exactly when `brief_type` is `product_demo`.
    pub demo_steps: Vec<DemoStep>,
    /// Compare-and-swap revision. Every write bumps it.
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// `typed_brief.get` input.
#[derive(Debug, Clone, Deserialize)]
pub struct TypedBriefGetInput {
    pub scope: Scope,
    pub brief_id: String,
}

/// `typed_brief.list` input: every brief in one scope.
#[derive(Debug, Clone, Deserialize)]
pub struct TypedBriefListInput {
    pub scope: Scope,
}

/// `typed_brief.list` output, ordered by brief id ascending.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypedBriefListOutput {
    pub scope: Scope,
    pub briefs: Vec<TypedBrief>,
}

/// One platform-native variant of one brief at one brief revision, under one catalog revision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BriefVariant {
    pub scope: Scope,
    pub variant_id: String,
    pub brief_id: String,
    pub brief_revision: i64,
    pub platform: String,
    /// The catalog revision id (`platform:hash`) the variant was checked under.
    pub rule_revision: String,
    /// The placement the catalog applied: one of `LINK_PLACEMENTS`.
    pub link_placement: String,
    /// The final caption text: body, then the link when placed in the caption, then the hashtags.
    pub caption: String,
    /// The https link as given, when one was given.
    pub link: Option<String>,
    /// The link, when the rule places it in the first comment.
    pub first_comment: Option<String>,
    /// The link, when the rule places it in the bio.
    pub bio_link: Option<String>,
    pub created_at: String,
}

/// `variant.derive` input. One variant per platform, in the order given. `hashtags` are words, with or
/// without a leading `#`. `duration_seconds` is the media length when the variant carries media.
#[derive(Debug, Clone, Deserialize)]
pub struct VariantDeriveInput {
    pub scope: Scope,
    pub brief_id: String,
    pub platforms: Vec<String>,
    #[serde(default)]
    pub hashtags: Vec<String>,
    #[serde(default)]
    pub link: Option<String>,
    #[serde(default)]
    pub duration_seconds: Option<i64>,
    /// Refuse a platform whose catalog rules were fetched more than this many days before `now` (`rule_stale`).
    /// Null applies no age limit.
    #[serde(default)]
    pub max_rule_age_days: Option<i64>,
    #[serde(default)]
    pub now: Option<String>,
}

/// One derived variant. `created` is false when the same inputs already produced it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DerivedVariant {
    pub variant: BriefVariant,
    pub created: bool,
}

/// `variant.derive` output, in the platform order given.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantDeriveOutput {
    pub scope: Scope,
    pub brief_id: String,
    pub variants: Vec<DerivedVariant>,
}

/// `variant.list` input: the variants of one brief in one scope.
#[derive(Debug, Clone, Deserialize)]
pub struct VariantListInput {
    pub scope: Scope,
    pub brief_id: String,
}

/// `variant.list` output, ordered by platform then variant id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantListOutput {
    pub scope: Scope,
    pub brief_id: String,
    pub variants: Vec<BriefVariant>,
}
