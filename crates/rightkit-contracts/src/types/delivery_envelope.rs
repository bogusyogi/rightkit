use serde::{Deserialize, Serialize};

/// `delivery-envelope` v1. The media-neutral hand-off that the daemon consumes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DeliveryEnvelope {
    pub contract: String,
    pub contract_version: String,
    pub envelope_id: String,
    pub brief_id: String,
    pub brief_revision: u64,
    pub content_revision: u64,
    pub policy_revision: u64,
    /// Platform rule revision the artifact was validated against (contract 1.1.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_revision: Option<u64>,
    /// Platform rules revision as `<platform>:<hash>`, the `Display` form of
    /// `rightkit-platform-rules` `RuleRevision` (contract 1.2.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_revision_ref: Option<String>,
    pub producer: Producer,
    pub platforms: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy: Option<EnvelopeCopy>,
    pub payload: Payload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Producer {
    pub engine: Engine,
    pub engine_version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    Cutright,
    Genright,
    Postright,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EnvelopeCopy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    #[serde(default)]
    pub hashtags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_placement: Option<LinkPlacement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkPlacement {
    Caption,
    Comment,
    Bio,
    None,
}

/// The payload is discriminated by `type`. The
/// `type` tag is the only discriminator, and the schema forbids other keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Payload {
    Video {
        media: VideoMedia,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        captions: Option<CaptionTrack>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thumbnail: Option<Still>,
    },
    Stills {
        stills: Vec<Still>,
    },
    Carousel {
        slides: Vec<Slide>,
    },
    Text {
        body: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        locale: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct VideoMedia {
    pub hash: String,
    pub container: Container,
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub duration_ms: u64,
    pub aspect: Aspect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<Audio>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Container {
    Mp4,
    Mov,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    H264,
    Hevc,
    Prores,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Aspect {
    #[serde(rename = "16:9")]
    SixteenNine,
    #[serde(rename = "9:16")]
    NineSixteen,
    #[serde(rename = "1:1")]
    OneOne,
    #[serde(rename = "4:5")]
    FourFive,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Audio {
    pub codec: String,
    pub channels: u8,
    pub sample_rate: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loudness_lufs: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptionTrack {
    pub format: CaptionFormat,
    pub hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptionFormat {
    Srt,
    Vtt,
}

/// A still image. `alt` is required on every still, so a carousel or a stills
/// post is never published without alt text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Still {
    pub hash: String,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub alt: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Slide {
    pub index: u32,
    pub still: Still,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slide_copy: Option<String>,
    #[serde(default)]
    pub step_ids: Vec<String>,
}
