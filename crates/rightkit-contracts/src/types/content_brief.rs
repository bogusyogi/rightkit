use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `content-brief` v1. One immutable revision of a brief.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ContentBrief {
    pub contract: String,
    pub contract_version: String,
    pub brief_id: String,
    pub revision: u64,
    #[serde(default)]
    pub supersedes: Option<u64>,
    #[serde(default)]
    pub created_at: Option<String>,
    pub brief_type: BriefType,
    pub channel: Channel,
    pub platforms: Vec<String>,
    pub deliverables: Vec<Deliverable>,
    #[serde(default)]
    pub hook: Option<String>,
    #[serde(default)]
    pub script: Option<String>,
    #[serde(default)]
    pub references: Vec<String>,
    #[serde(default)]
    pub facts: Vec<Fact>,
    #[serde(default)]
    pub review: Option<Review>,
    pub stages: Vec<Stage>,
    #[serde(default)]
    pub scenario: Option<Scenario>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BriefType {
    TalkingHead,
    FacelessCartoon,
    Explainer,
    Carousel,
    UgcCharacterReview,
    ProductPitch,
    ProductDemo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Deliverable {
    Video,
    Stills,
    Carousel,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageId {
    Research,
    Script,
    Generate,
    Record,
    Align,
    Cut,
    Voice,
    Carousel,
    Manifest,
    Upload,
    Publish,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Channel {
    pub venture: String,
    pub channel: String,
    pub account: String,
}

/// A stage's identity: its stable request id and the hash of its inputs. A
/// corrected brief changes the input hash, so stale assets are never reused.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Stage {
    pub id: StageId,
    pub request_id: String,
    pub input_hash: String,
    pub depends_on: Vec<StageId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Fact {
    pub fact_id: String,
    pub text: String,
    pub status: FactStatus,
    #[serde(default)]
    pub proof_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactStatus {
    Unresolved,
    Proven,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewMode {
    Manual,
    AutoWhenClean,
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Review {
    pub mode: ReviewMode,
    #[serde(default)]
    pub policy_revision: Option<u64>,
}

/// The recording scenario a `product_demo` brief carries as data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Scenario {
    pub scenario_id: String,
    pub app: ScenarioApp,
    pub runner: Runner,
    pub fps: u32,
    pub targets: Vec<Target>,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ScenarioApp {
    pub app_id: String,
    #[serde(default)]
    pub bundle_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Runner {
    pub host: RunnerHost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerHost {
    MacSecondSession,
    WindowsSession,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Target {
    pub target_id: String,
    pub role: String,
    pub label: String,
}

/// One scenario step. `say` is the narration text for the step; the director
/// anchors zooms and callouts to the word that names the control.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Step {
    pub step_id: String,
    pub label: String,
    pub action: Action,
    pub expect: Expect,
    #[serde(default)]
    pub say: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Click,
    RightClick,
    Type,
    Key,
    Scroll,
    Drag,
    Wait,
    Navigate,
    Menu,
    Focus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Action {
    pub kind: ActionKind,
    #[serde(default)]
    pub target_id: Option<String>,
    #[serde(default)]
    pub params: Option<Value>,
}

/// The UI change the step waits for. The runner observes this; it never assumes
/// the input itself succeeded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Expect {
    pub postcondition: String,
    #[serde(default)]
    pub timeout_ms: Option<u32>,
}
