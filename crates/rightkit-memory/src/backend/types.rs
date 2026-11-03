//! Stable values shared by embedded and resident memory backends.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type JsonValue = Value;
pub type MemoryId = String;
pub type AccessCount = u32;
pub type Score = f64;

/// Preserve Cortex's stable scope encoding at the backend boundary so callers
/// do not fork existing memories while migrating away from concrete store APIs.
pub fn normalize_scope(scope: &str) -> String {
    let bytes = scope.as_bytes();
    if bytes.len() >= 2
        && bytes[1] == b'-'
        && bytes[0].is_ascii_alphabetic()
        && bytes[0].is_ascii_lowercase()
    {
        let mut normalized = String::with_capacity(scope.len());
        normalized.push((bytes[0] as char).to_ascii_uppercase());
        normalized.push_str(&scope[1..]);
        normalized
    } else {
        scope.to_string()
    }
}

/// Convert filesystem paths to Cortex's canonical project-scope slug.
pub fn path_to_scope(path: &str) -> String {
    let path = path.trim();
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    let slug: String = path
        .chars()
        .map(|character| {
            if matches!(character, ':' | '\\' | '/') {
                '-'
            } else {
                character
            }
        })
        .collect();
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "workspace".to_string()
    } else {
        normalize_scope(slug)
    }
}

/// Return self, existing ancestors, then global without admitting siblings.
pub fn scope_chain(scope_id: &str, existing: &[String]) -> Vec<String> {
    let parts: Vec<&str> = scope_id.split('-').collect();
    let mut candidates = Vec::with_capacity(parts.len() + 2);
    candidates.push(scope_id.to_string());
    for index in (1..=parts.len()).rev() {
        candidates.push(parts[..index].join("-"));
    }
    let mut chain = Vec::new();
    for candidate in candidates {
        if !chain.contains(&candidate)
            && (candidate == scope_id || existing.iter().any(|scope| scope == &candidate))
        {
            chain.push(candidate);
        }
    }
    if existing.iter().any(|scope| scope == "global")
        && !chain.iter().any(|scope| scope == "global")
    {
        chain.push("global".to_string());
    }
    chain
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryTier {
    Working,
    Episodic,
    Semantic,
}

impl MemoryTier {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Working => "Working",
            Self::Episodic => "Episodic",
            Self::Semantic => "Semantic",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "working" => Some(Self::Working),
            "episodic" => Some(Self::Episodic),
            "semantic" => Some(Self::Semantic),
            _ => None,
        }
    }
}

/// The route-visible memory row. Field names match the stable resident wire shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: MemoryId,
    pub tier: MemoryTier,
    pub content: String,
    pub keywords: Vec<String>,
    pub score: Score,
    pub created_at: String,
    pub access_count: AccessCount,
    #[serde(default)]
    pub embedding: Option<Vec<f32>>,
    #[serde(default = "default_scope")]
    pub scope_id: String,
}

fn default_scope() -> String {
    "global".to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FullMemoryRecord {
    pub id: MemoryId,
    pub content: String,
    pub access_count: AccessCount,
    #[serde(default)]
    pub lifecycle: Option<JsonValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoredMemoryEntry {
    pub entry: MemoryEntry,
    pub score: Score,
}

/// Row returned by the legacy `/list` route, retained until its route adapter is cut over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryListRow {
    pub id: MemoryId,
    pub tier: MemoryTier,
    pub chars: usize,
    pub access_count: AccessCount,
    pub inject_count: AccessCount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackendKind {
    Embedded,
    Service,
    Explicit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendIdentity {
    pub backend_id: String,
    pub kind: BackendKind,
    pub protocol_version: u32,
    pub schema_version: u32,
    pub release_generation: Option<String>,
}

impl BackendIdentity {
    pub fn embedded(id: impl Into<String>) -> Self {
        Self {
            backend_id: id.into(),
            kind: BackendKind::Embedded,
            protocol_version: 1,
            schema_version: 1,
            release_generation: None,
        }
    }

    pub fn service(
        id: impl Into<String>,
        protocol_version: u32,
        schema_version: u32,
        release_generation: Option<String>,
    ) -> Self {
        Self {
            backend_id: id.into(),
            kind: BackendKind::Service,
            protocol_version,
            schema_version,
            release_generation,
        }
    }

    pub fn explicit(
        id: impl Into<String>,
        protocol_version: u32,
        schema_version: u32,
        release_generation: Option<String>,
    ) -> Self {
        Self {
            backend_id: id.into(),
            kind: BackendKind::Explicit,
            protocol_version,
            schema_version,
            release_generation,
        }
    }
}

/// Immutable binding exposed to diagnostics and selectors after startup selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendBinding {
    identity: BackendIdentity,
}

impl BackendBinding {
    pub fn new(identity: BackendIdentity) -> Self {
        Self { identity }
    }

    pub fn identity(&self) -> &BackendIdentity {
        &self.identity
    }

    pub fn kind(&self) -> BackendKind {
        self.identity.kind
    }
}
