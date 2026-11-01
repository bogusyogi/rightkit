use crate::OverlayIdentityV1;
use serde::{Deserialize, Serialize};
pub const SOURCE_RESOLUTION_RECEIPT_SCHEMA_VERSION: u32 = 1;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceResolutionStatusV1 {
    Resolved,
    Unresolved,
    HashMismatch,
    GenerationMismatch,
    PathMismatch,
    ResolverUnavailable,
    MissingIdentity,
    MissingProvider,
    MissingHash,
    MissingGeneration,
    MissingPath,
}
impl SourceResolutionStatusV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::Unresolved => "unresolved",
            Self::HashMismatch => "hash_mismatch",
            Self::GenerationMismatch => "generation_mismatch",
            Self::PathMismatch => "path_mismatch",
            Self::ResolverUnavailable => "resolver_unavailable",
            Self::MissingIdentity => "missing_identity",
            Self::MissingProvider => "missing_provider",
            Self::MissingHash => "missing_hash",
            Self::MissingGeneration => "missing_generation",
            Self::MissingPath => "missing_path",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceResolutionReceiptV1 {
    pub schema_version: u32,
    pub candidate_id: String,
    pub provider: String,
    pub status: SourceResolutionStatusV1,
    pub expected_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_hash: Option<String>,
    pub expected_generation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_generation: Option<String>,
    pub expected_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_path: Option<String>,
    pub resolver: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay_identity: Option<OverlayIdentityV1>,
}
impl SourceResolutionReceiptV1 {
    pub fn is_exact_current_source(&self) -> bool {
        self.schema_version == SOURCE_RESOLUTION_RECEIPT_SCHEMA_VERSION
            && self.status == SourceResolutionStatusV1::Resolved
            && !self.candidate_id.is_empty()
            && !self.provider.is_empty()
            && valid_source_digest(&self.expected_hash)
            && self.resolved_hash.as_deref() == Some(self.expected_hash.as_str())
            && !self.expected_generation.is_empty()
            && self.resolved_generation.as_deref() == Some(self.expected_generation.as_str())
            && !self.expected_path.is_empty()
            && self.resolved_path.as_deref() == Some(self.expected_path.as_str())
            && !self.resolver.is_empty()
    }
}
#[rustfmt::skip] fn valid_source_digest(value: &str) -> bool {
    let (prefix, hex_len) = if value.starts_with("sha256:") { ("sha256:", 64) } else if value.starts_with("xxh128:") { ("xxh128:", 32) } else { return false; };
    value.len() == prefix.len() + hex_len && value[prefix.len()..].bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
}
