use serde::{Deserialize, Serialize};

/// A verdict attached by a juror. Honored only while `bound_hash` == the artifact's current content hash (A4).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactVerdict {
    pub passed: bool,
    pub juror: String,
    pub bound_hash: String,
    pub summary: String,
}

/// A recorded approval. Honored only while `bound_hash` == the artifact's current content hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactApproval {
    pub approver: String,
    pub bound_hash: String,
    pub created_at: String,
}

/// A trust-bearing artifact record (P2 §15). The trust fields (shippable, verdict, approvals) are
/// only settable through the gated `artifact_trust` tools (A3).
///
/// `trust_label` is a storage-level trust propagation field (Row 12 follow-up).
/// Defaults to `"unreviewed"`; the `artifact_trust` tool family is the only
/// legitimate way to change it post-creation. Downstream consumers
/// (compaction, evidence graph, runtime panels) read the label to gate
/// which contexts may see the artifact body.
///
/// Note: `effective_verdict` and `effective_approvals` (A4 hash-binding) are computed properties
/// in C# marked `[JsonIgnore]` — they are not serialized and have no Rust equivalent here.
/// Consumers that need hash-binding logic should implement it themselves using `content_hash`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRow {
    pub id: String,
    #[serde(default)]
    pub path: String,
    pub kind: String,
    pub content_hash: String,
    pub shippable: bool,
    pub producer: String,
    #[serde(default)]
    pub provenance: String,
    #[serde(default = "default_artifact_trust_label")]
    pub trust_label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<ArtifactVerdict>,
    #[serde(default)]
    pub approvals: Vec<ArtifactApproval>,
    #[serde(default)]
    pub meta: String,
    #[serde(default)]
    pub created_at: String,
}

fn default_artifact_trust_label() -> String {
    "unreviewed".to_string()
}

/// CRUD-surface bookkeeping for an [`ArtifactRow`] (ENG-115), carried inside
/// its opaque `meta` JSON string rather than as new `ArtifactRow` fields.
///
/// `ArtifactRow` is constructed via exhaustive struct literals in several
/// crates outside this lane's file allowlist (`storage::artifacts`,
/// `host-app/src/api_session_utils_artifacts.rs`, and others) with no
/// `..Default::default()` spread. Adding a new required field there would be
/// a breaking change this lane cannot land, since fixing every call site is
/// out of scope. Piggybacking on `meta` — already a freeform per-artifact
/// JSON blob other producers (e.g. the browser-artifact writer) use the same
/// way — keeps `artifact_crud` additive.
///
/// A consumer that needs a typed edit counter or "session that produced
/// this" field on `ArtifactRow` itself should thread it through every
/// existing constructor in the same change, not add it silently.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactCrudMeta {
    /// Monotonic content version. Starts at 1 on creation and is incremented
    /// by exactly one on every `artifact_update` that lands. Distinct from
    /// `content_hash`-based trust binding: this is a plain edit counter the
    /// CRUD surface's optimistic-concurrency check compares cheaply without
    /// hashing.
    #[serde(default = "default_artifact_crud_version")]
    pub version: u64,
    /// Wall-clock stamp (ISO-8601) of the most recent create/update.
    #[serde(default)]
    pub updated_at: String,
    /// Session that produced this artifact through the CRUD surface.
    #[serde(default)]
    pub session_id: String,
}

fn default_artifact_crud_version() -> u64 {
    1
}

impl Default for ArtifactCrudMeta {
    fn default() -> Self {
        Self {
            version: default_artifact_crud_version(),
            updated_at: String::new(),
            session_id: String::new(),
        }
    }
}

impl ArtifactCrudMeta {
    /// Parse from `ArtifactRow::meta`. Any non-JSON or unrecognized shape
    /// (e.g. another producer's freeform `meta`) yields the version-1
    /// default rather than an error — this is bookkeeping, not a contract
    /// other producers must satisfy.
    pub fn from_meta_str(meta: &str) -> Self {
        serde_json::from_str(meta).unwrap_or_default()
    }

    pub fn to_meta_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}
