//! Requested media output: what to fetch and at which quality (wire-compatible serde shapes).

use serde::{Deserialize, Serialize};

/// What a media job should produce.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MediaAction {
    #[default]
    Transcribe,
    Download,
    Both,
    /// Save the post's images (single image, carousel files) without transcribing (#248).
    Images,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
/// Requested video quality.
pub enum VideoQuality {
    P720,
    #[default]
    P1080,
    P2160,
    Source,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
/// Requested audio quality.
pub enum AudioQuality {
    #[default]
    Best,
    Compact,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
/// The user-requested output of one media job.
pub struct MediaSpec {
    #[serde(default)]
    pub action: MediaAction,
    #[serde(default)]
    pub video: Option<VideoQuality>,
    #[serde(default)]
    pub audio: Option<AudioQuality>,
}
