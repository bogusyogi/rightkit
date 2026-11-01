// Dimension types + dispatcher. Included from `doctor.rs`. The five probes
// themselves live in `doctor_dimension_probes.rs` (another include! sibling).

use std::collections::BTreeMap;

/// One security/runtime dimension probe result.
#[derive(Debug, Clone, Serialize)]
pub struct Dimension {
    /// `pass` | `warn` | `block` | `unknown` — lowercase for stable JSON keys.
    pub status: &'static str,
    /// Human-readable summary of what was found.
    pub detail: String,
    /// Per-dimension evidence (only present with `--verbose` and non-empty).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Vec<Evidence>>,
}

/// One piece of evidence inside a dimension — used for `--verbose`.
#[derive(Debug, Clone, Serialize)]
pub struct Evidence {
    pub id: String,
    pub label: String,
    /// `pass` | `warn` | `block` | `unknown`
    pub status: &'static str,
    pub detail: String,
}

pub(crate) fn dim(status: &'static str, detail: impl Into<String>) -> Dimension {
    Dimension {
        status,
        detail: detail.into(),
        evidence: None,
    }
}

pub(crate) fn dim_with_evidence(
    status: &'static str,
    detail: impl Into<String>,
    evidence: Vec<Evidence>,
) -> Dimension {
    Dimension {
        status,
        detail: detail.into(),
        // Skip empty arrays so verbose mode doesn't emit `"evidence": []` for
        // dimensions with no sub-checks.
        evidence: if evidence.is_empty() {
            None
        } else {
            Some(evidence)
        },
    }
}

/// Collect every dimension probe into a stable-key map. Output order in JSON
/// (alphabetical via `BTreeMap`) is part of the `coderight.cli.doctor.v1`
/// contract — downstream consumers MAY rely on it.
pub fn collect_dimensions(verbose: bool) -> BTreeMap<&'static str, Dimension> {
    let mut map = BTreeMap::new();
    map.insert("csprng", probe_csprng(verbose));
    map.insert("hooks", probe_hooks(verbose));
    map.insert("learning", probe_learning(verbose));
    map.insert("policy", probe_policy(verbose));
    map.insert("workspace-trust", probe_workspace_trust(verbose));
    map
}
