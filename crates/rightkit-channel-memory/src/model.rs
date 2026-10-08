//! Typed mirror of channel-memory.v1. Field names follow the JSON; `ref` is
//! renamed because it is a Rust keyword.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Scope {
    pub venture: String,
    pub channel: String,
    pub account: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Source {
    pub kind: SourceKind,
    #[serde(rename = "ref")]
    pub reference: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    RoadmapFile,
    HookBank,
    AdPattern,
    Website,
    ConnectedAccount,
    Interview,
    AnalyticsExport,
    ReelDecoder,
    AdrianCorrection,
    Daemon,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Voice {
    pub tone: String,
    pub audience: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narration: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct VisualSystem {
    pub style: String,
    pub layout: String,
    pub caption_style: String,
    pub pacing: String,
    pub sound: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typography: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub palette: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Pillar {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Batch {
    Overnight,
    Daytime,
    #[serde(rename = "none")]
    NoBatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BatchWindow {
    pub start: String,
    pub end: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Cadence {
    pub posts_per_day: u32,
    pub timezone: String,
    pub batch: Batch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_window: Option<BatchWindow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Guidance {
    pub do_list: Vec<String>,
    pub dont_list: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppliesTo {
    Hook,
    Script,
    Caption,
    Carousel,
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BannedMove {
    pub id: String,
    pub description: String,
    pub applies_to: AppliesTo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetRights {
    CommercialSafe,
    Licensed,
    EditorialOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LicenceScope {
    pub monetised: bool,
    pub asset_rights: AssetRights,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProofOrigin {
    Interview,
    ConnectedAccount,
    RoadmapFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProofSourceKind {
    Interview,
    ConnectedAccount,
    RoadmapFile,
    AdrianCorrection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProofSource {
    pub kind: ProofSourceKind,
    #[serde(rename = "ref")]
    pub reference: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Approved,
    Pending,
    Withdrawn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Approval {
    pub status: ApprovalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_at: Option<String>,
}

/// An authoritative fact. Only proofs may be cited as facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Proof {
    pub id: String,
    pub subject: String,
    pub value: f64,
    pub units: String,
    pub observed_from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_to: Option<String>,
    pub source_excerpt: String,
    pub origin: ProofOrigin,
    pub source: ProofSource,
    pub approval: Approval,
    pub permitted_claims: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationOrigin {
    Website,
    PlatformText,
    Scrape,
}

/// External text. Never authoritative; never citable as a fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Observation {
    pub id: String,
    pub origin: ObservationOrigin,
    pub text: String,
    pub observed_at: String,
    pub authoritative: AlwaysFalse,
    pub source: Source,
}

/// Serialises as `false` and refuses `true` on read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AlwaysFalse;

impl Serialize for AlwaysFalse {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bool(false)
    }
}

impl<'de> Deserialize<'de> for AlwaysFalse {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = bool::deserialize(deserializer)?;
        if value {
            Err(serde::de::Error::custom(
                "observation authoritative must be false",
            ))
        } else {
            Ok(AlwaysFalse)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Lesson {
    pub id: String,
    pub text: String,
    pub recorded_at: String,
    pub source: Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    Views,
    #[serde(rename = "retention_30s")]
    Retention30s,
    AverageViewDuration,
    Saves,
    Shares,
    Comments,
    Follows,
    LinkClicks,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DateWindow {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PerformanceNote {
    pub id: String,
    pub platform: String,
    pub metric: Metric,
    pub window: DateWindow,
    pub sample_size: u32,
    pub summary: String,
    pub caveat: String,
    pub source: Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceKind {
    Reel,
    Screenshot,
    Website,
    Ad,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StyleNotes {
    #[serde(rename = "type")]
    pub kind: String,
    pub caption_style: String,
    pub layout: String,
    pub pacing: String,
    pub sound: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub palette: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Reference {
    pub id: String,
    pub kind: ReferenceKind,
    pub style_notes: StyleNotes,
    pub source: Source,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModelDefaults {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub music: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CharacterStatus {
    Active,
    Retired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CharacterBinding {
    pub id: String,
    pub anchor_asset_id: String,
    pub status: CharacterStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProductReference {
    pub id: String,
    pub asset_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Pronunciation {
    pub term: String,
    pub respelling: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct VoiceBinding {
    pub voice_id: String,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pronunciation: Option<Vec<Pronunciation>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrandAssetRole {
    Logo,
    Font,
    ColourReference,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BrandAsset {
    pub id: String,
    pub asset_id: String,
    pub role: BrandAssetRole,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct IdentityBindings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub characters: Option<Vec<CharacterBinding>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_references: Option<Vec<ProductReference>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<VoiceBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brand_assets: Option<Vec<BrandAsset>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthoredBy {
    Adrian,
    Seed,
    Decoder,
    Skill,
    Daemon,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Provenance {
    pub authored_by: AuthoredBy,
    pub sources: Vec<Source>,
}

/// One immutable revision of a channel's memory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ChannelMemory {
    pub contract: String,
    pub contract_version: String,
    pub memory_id: String,
    pub revision: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<u32>,
    pub created_at: String,
    pub scope: Scope,
    pub brand_code: String,
    pub voice: Voice,
    pub visual_system: VisualSystem,
    pub pillars: Vec<Pillar>,
    pub cadence: Cadence,
    pub guidance: Guidance,
    pub banned_moves: Vec<BannedMove>,
    pub banned_phrases: Vec<String>,
    pub licence_scope: LicenceScope,
    pub proofs: Vec<Proof>,
    pub observations: Vec<Observation>,
    pub lessons: Vec<Lesson>,
    pub performance_notes: Vec<PerformanceNote>,
    pub references: Vec<Reference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_defaults: Option<ModelDefaults>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_bindings: Option<IdentityBindings>,
    pub provenance: Provenance,
}
