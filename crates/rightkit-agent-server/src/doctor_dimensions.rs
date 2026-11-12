// Dimension types + dispatcher. Included from `doctor.rs`. The five probes
// themselves live in `doctor_dimension_probes.rs` (another include! sibling).

use std::collections::BTreeMap;

pub use rightkit_doctor::verdict::{Dimension, Evidence};

pub(crate) fn dim(status: &'static str, detail: impl Into<String>) -> Dimension {
    Dimension::new(status, detail)
}

pub(crate) fn dim_with_evidence(
    status: &'static str,
    detail: impl Into<String>,
    evidence: Vec<Evidence>,
) -> Dimension {
    Dimension::with_evidence(status, detail, evidence)
}

/// Collect every dimension probe into a stable-key map. Output order in JSON
/// (alphabetical via `BTreeMap`) is part of the `{app}.cli.doctor.v1`
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
