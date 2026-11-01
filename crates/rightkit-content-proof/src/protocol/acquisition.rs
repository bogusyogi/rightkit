//! Payload shapes for the P3 acquisition chain (docs/contracts/p3-acquisition-gates.md). PostRight's side only:
//! an acquisition request behind a recorded approval, the import of a ScrapeRight result, counter provenance,
//! the label store for the reel decoder, correlation rows and the playbook. Nothing here calls ScrapeRight.
use serde::Deserialize;
use serde_json::Value;

use super::Scope;

/// Platforms a creator may be on (contract 2.1).
pub const ACQ_PLATFORMS: [&str; 3] = ["instagram", "youtube", "tiktok"];

/// Counter names (contract 2.4).
pub const ACQ_COUNTER_NAMES: [&str; 6] = ["plays", "views", "likes", "comments", "shares", "saves"];

/// Counter provenance methods. Only the first two may be correlated with plays; the third is research medians only
/// (decision 1); any other method (an analytics page) is stored as forbidden and used for nothing.
pub const ACQ_METHOD_OFFICIAL: &str = "official_api_field";
pub const ACQ_METHOD_EXPORT: &str = "user_export_row";
pub const ACQ_METHOD_PUBLIC: &str = "public_page_text";

/// The eight labels of the reel decoder (contract 2.6).
pub const ACQ_LABEL_KEYS: [&str; 8] = [
    "hook",
    "structure",
    "opening",
    "evidence",
    "emotion",
    "advice",
    "topic",
    "cta",
];

/// Fewest items with permitted counters before a median is written (decision 2). A caller may raise it, never lower it.
pub const ACQ_MIN_SAMPLE: i64 = 10;

/// Days raw reference media is kept after download (decision 5).
pub const ACQ_RETENTION_DAYS: i64 = 30;

/// A creator reference: platform, handle and an https profile URL.
#[derive(Debug, Clone, Deserialize)]
pub struct AcqCreator {
    pub platform: String,
    pub handle: String,
    #[serde(rename = "profileUrl")]
    pub profile_url: String,
}

/// A date window `YYYY-MM-DD` to `YYYY-MM-DD`.
#[derive(Debug, Clone, Deserialize)]
pub struct AcqWindow {
    pub from: String,
    pub to: String,
}

/// `acquisition.approve`: Adrian's explicit approval of one live run: creator, item count and date window.
/// `approval_ref` names where he gave it (a quoted message or a chat reference); both are required.
#[derive(Debug, Clone, Deserialize)]
pub struct AcqApproveInput {
    pub scope: Scope,
    pub creator: AcqCreator,
    pub window: AcqWindow,
    #[serde(rename = "maxItems")]
    pub max_items: i64,
    pub approved_by: String,
    pub approval_ref: String,
    #[serde(default)]
    pub now: Option<String>,
}

/// `acquisition.request`: stage 1. Refused without a recorded approval that matches creator, window and item count.
#[derive(Debug, Clone, Deserialize)]
pub struct AcqRequestInput {
    pub scope: Scope,
    pub creator: AcqCreator,
    pub window: AcqWindow,
    #[serde(rename = "maxItems")]
    pub max_items: i64,
    /// `none` or `public_page_text`.
    pub counters: String,
    #[serde(default)]
    pub approval_id: Option<String>,
    /// Only `none` is accepted: no cookies from any signed-in profile (decision 6).
    #[serde(default)]
    pub cookies: Option<String>,
    #[serde(default)]
    pub now: Option<String>,
}

/// `acquisition.import`: a ScrapeRight catalogue result (R-SR-1 response) for a stored request. Fixture or live.
#[derive(Debug, Clone, Deserialize)]
pub struct AcqImportInput {
    pub scope: Scope,
    pub request_id: String,
    pub result: Value,
    #[serde(default)]
    pub now: Option<String>,
}

/// `acquisition.media_import`: media records of an R-SR-2 response.
#[derive(Debug, Clone, Deserialize)]
pub struct AcqMediaImportInput {
    pub scope: Scope,
    pub platform: String,
    pub media: Vec<Value>,
    #[serde(default)]
    pub now: Option<String>,
}

/// `acquisition.transcript_import`: one result of a `transcript.batch` response for an item.
#[derive(Debug, Clone, Deserialize)]
pub struct AcqTranscriptImportInput {
    pub scope: Scope,
    pub platform: String,
    pub item_id: String,
    pub media_blake3: String,
    pub result: Value,
}

/// `acquisition.label_put`: the eight labels of one item, each grounded in a transcript quote.
#[derive(Debug, Clone, Deserialize)]
pub struct AcqLabelPutInput {
    pub scope: Scope,
    pub platform: String,
    pub item_id: String,
    pub labels: Value,
    pub labeler: Value,
    #[serde(default)]
    pub now: Option<String>,
}

/// `acquisition.correlate`: write the correlation row of one item. Refused unless its plays counter is permitted.
#[derive(Debug, Clone, Deserialize)]
pub struct AcqCorrelateInput {
    pub scope: Scope,
    pub platform: String,
    pub item_id: String,
    #[serde(default)]
    pub now: Option<String>,
}

/// `acquisition.playbook`: build the playbook of one catalogue request. Medians only at `minimum_sample` items or more.
#[derive(Debug, Clone, Deserialize)]
pub struct AcqPlaybookInput {
    pub scope: Scope,
    pub request_id: String,
    pub niche: String,
    #[serde(default)]
    pub minimum_sample: Option<i64>,
    /// When true, a sample below the minimum is the error `insufficient_sample` and nothing is written.
    #[serde(default)]
    pub require_medians: bool,
    #[serde(default)]
    pub now: Option<String>,
}

/// `acquisition.playbook_get`.
#[derive(Debug, Clone, Deserialize)]
pub struct AcqPlaybookGetInput {
    pub scope: Scope,
    pub playbook_id: String,
}

/// `acquisition.prune`: delete raw media whose retention passed. `now` is the clock.
#[derive(Debug, Clone, Deserialize)]
pub struct AcqPruneInput {
    pub scope: Scope,
    #[serde(default)]
    pub now: Option<String>,
}

/// `acquisition.media_use`: offer a media record to a brief, GenRight, an export or a publish payload.
#[derive(Debug, Clone, Deserialize)]
pub struct AcqMediaUseInput {
    pub scope: Scope,
    pub media_id: String,
    /// `brief`, `genright`, `cutright`, `export` or `publish`.
    pub target: String,
}

/// `acquisition.visual_extract`: palette, layout or scene extraction from reference stills (blocked: contract 9).
#[derive(Debug, Clone, Deserialize)]
pub struct AcqVisualInput {
    pub scope: Scope,
    #[serde(default)]
    pub stills: Vec<String>,
}

/// The stages of the acquisition chain after creator intake, in order (contract 1). Each has stage input hashes and a
/// stable per-stage request id (contract 4).
pub const ACQ_STAGES: [&str; 6] = [
    "catalogue",
    "media",
    "transcript",
    "labels",
    "correlation",
    "playbook",
];

/// `acquisition.stage_run`: run the stages of one catalogue request in order, up to `stage` (default all). A stage whose
/// input hash matches a stored `ok` record does no work; a changed input re-runs that stage and everything after it.
#[derive(Debug, Clone, Deserialize)]
pub struct AcqStageRunInput {
    pub scope: Scope,
    /// The catalogue request id (`req.catalogue.<16 hex>`) that names the run.
    pub request_id: String,
    /// Run up to and including this stage. Default: `playbook`.
    #[serde(default)]
    pub stage: Option<String>,
    /// Playbook niche; required when the run reaches the playbook stage. Part of the playbook stage input.
    #[serde(default)]
    pub niche: Option<String>,
    #[serde(default)]
    pub minimum_sample: Option<i64>,
    /// Continue past a `partial` catalogue (default false: the run stops at the catalogue until it is complete).
    #[serde(default)]
    pub allow_partial: bool,
    /// Stop cleanly after this stage completes. A stand-in for a process kill between stages, for fixtures.
    #[serde(default)]
    pub halt_after: Option<String>,
    #[serde(default)]
    pub now: Option<String>,
}

/// `acquisition.stage_status`: per stage, the current input hash, request id and whether a stored record covers it.
#[derive(Debug, Clone, Deserialize)]
pub struct AcqStageStatusInput {
    pub scope: Scope,
    pub request_id: String,
    #[serde(default)]
    pub niche: Option<String>,
    #[serde(default)]
    pub minimum_sample: Option<i64>,
}
