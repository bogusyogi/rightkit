use serde::{Deserialize, Serialize};

pub const TOOL_OUTPUT_INTERNAL_KIND: &str = "tool_output_internal";

/// A reference to a (possibly large) stored output kept out of the transcript so context stays bounded.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputRef {
    pub id: String,
    /// e.g. "tool_output", "file", "shell_stdout"
    pub kind: String,
    pub size_bytes: i64,
    pub content_hash: Option<String>,
    pub preview: Option<String>,
    /// MIME type of the output envelope, independent of whether it is inline
    /// or externalized.
    #[serde(default = "default_output_media_type")]
    pub media_type: String,
    /// True when `preview` omits any part of the original output.
    #[serde(default)]
    pub truncated: bool,
    /// True only when `id` can be resolved through the authenticated output
    /// retrieval surface.
    #[serde(default)]
    pub retrievable: bool,
}

fn default_output_media_type() -> String {
    "text/plain; charset=utf-8".to_string()
}

/// Inert in P0/P1. A shell reference to a reviewable artifact for transcript display only.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRef {
    pub id: String,
    /// e.g. "plan" | "diff" | "test_report" | "screenshot" | "cost_report"
    pub kind: String,
    pub title: Option<String>,
    pub content_hash: Option<String>,
}
