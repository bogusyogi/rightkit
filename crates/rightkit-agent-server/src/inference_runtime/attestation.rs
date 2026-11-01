use super::InferenceSelection;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionFingerprintInput<'a> {
    pub schema: u8,
    pub authority: &'a str,
    pub connection_id: &'a str,
    pub protocol: &'a str,
    pub normalized_base_url: &'a str,
    pub auth_kind: &'a str,
    pub auth_header_name: Option<&'a str>,
    pub credential_revision: u64,
    pub configuration_revision: u64,
    pub model_id: &'a str,
    pub route_policy_id: Option<&'a str>,
    pub catalog_entry_hash: &'a str,
    pub security_policy_hash: &'a str,
    pub tool_schema_hash: &'a str,
}
pub fn selection_fingerprint(input: &SelectionFingerprintInput<'_>) -> String {
    // Struct ordering is canonical field ordering; no credential material enters input.
    let json = serde_json::to_vec(input).unwrap_or_default();
    let mut hash = rightkit_fs::digest::Sha256Hasher::new();
    hash.update(b"coderight.inference-selection.v1\0");
    hash.update(AsRef::<[u8]>::as_ref(&json));
    hash.finalize()
}
pub fn selection_matches(
    stored: &InferenceSelection,
    current: &InferenceSelection,
    stored_fingerprint: &str,
    current_fingerprint: &str,
) -> bool {
    stored == current && stored_fingerprint == current_fingerprint
}
