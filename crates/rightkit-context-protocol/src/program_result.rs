//! Pull-owned bounded program-result reduction and exact recovery contract.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramResultActionV1 {
    Compress,
    Resolve,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProgramResultProtectedSpanV1 {
    pub start: usize,
    pub end: usize,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProgramResultRequestV1 {
    pub schema_version: u32,
    pub action: ProgramResultActionV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<usize>,
    #[serde(default)]
    pub protected_spans: Vec<ProgramResultProtectedSpanV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProgramResultDataV1 {
    pub schema_version: u32,
    pub action: ProgramResultActionV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_encoding: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_met: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform_version: Option<u32>,
    #[serde(default)]
    pub protected_spans: Vec<ProgramResultProtectedSpanV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_byte: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_byte: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fidelity: Option<String>,
}
