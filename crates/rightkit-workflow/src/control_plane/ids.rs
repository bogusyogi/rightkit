//! Stable, namespaced, deterministic ID generation for the workflow plane.
//!
//! Two production constraints drove this design:
//!
//! 1. **Visibility.** Every durable object (definition, version, trigger,
//!    run, node, attempt, wait, approval, effect, attention, delivery) gets
//!    a prefix-bound string id so operators can tell what they're looking
//!    at from a log line alone.
//! 2. **Determinism.** Test fixtures, journal replay, and projection rebuild
//!    must produce the **same** id for the **same** sequence, without
//!    pulling in a randomness source.

use serde::{Deserialize, Serialize};

/// Stable, prefix-bound ID for one workflow durable object.
///
/// The constructor never accepts a free-form string. Use [`StableId::new`]
/// or [`NamespacedId::next`] instead — they guarantee the prefix matches
/// the object class.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct StableId(String);

impl StableId {
    /// Construct a stable id from a `prefix_value` pair.
    pub fn new(prefix: Prefix, value: impl Into<String>) -> Self {
        let mut value = value.into();
        debug_assert!(
            !value.is_empty(),
            "workflow id value must be non-empty (prefix={:?})",
            prefix
        );
        let marker = format!("{}_", prefix.as_str());
        let repeated = format!("{marker}{marker}");
        while value.starts_with(&repeated) {
            value = value[marker.len()..].to_string();
        }
        if value.starts_with(&marker) {
            Self(value)
        } else {
            Self(format!("{marker}{value}"))
        }
    }

    /// Borrow the underlying string representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parse a stable id back into its `(Prefix, value)` parts.
    pub fn split(&self) -> Option<(Prefix, &str)> {
        // Match longest prefixes first because `wf_evt_`, `wf_attn_`, and
        // `wf_delivery_` contain an underscore themselves.
        for prefix in [
            Prefix::WorkflowDefinition,
            Prefix::WorkflowVersion,
            Prefix::WorkflowTrigger,
            Prefix::WorkflowOccurrence,
            Prefix::WorkflowRun,
            Prefix::NodeRun,
            Prefix::NodeAttempt,
            Prefix::Wait,
            Prefix::Approval,
            Prefix::EffectIntent,
            Prefix::AttentionItem,
            Prefix::Delivery,
            Prefix::TriggerEvent,
            Prefix::Claim,
            Prefix::Evidence,
        ] {
            let marker = format!("{}_", prefix.as_str());
            if let Some(value) = self.0.strip_prefix(&marker) {
                return Some((prefix, value));
            }
        }
        None
    }
}

impl std::fmt::Display for StableId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Opaque, typed wrapper for one workflow durable object class. Construction
/// requires a [`Prefix`], which prevents accidental cross-namespace mixing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct NamespacedId {
    inner: StableId,
}

impl NamespacedId {
    /// Build a namespaced id in `prefix` from a `value`.
    pub fn new(prefix: Prefix, value: impl Into<String>) -> Self {
        Self {
            inner: StableId::new(prefix, value),
        }
    }

    /// Borrow the stable id.
    pub fn stable(&self) -> &StableId {
        &self.inner
    }

    /// Borrow the prefix.
    pub fn prefix(&self) -> Prefix {
        // Safe: stable id is always constructed via a prefix.
        self.inner
            .split()
            .map(|(p, _)| p)
            .unwrap_or(Prefix::WorkflowDefinition)
    }
}

impl std::fmt::Display for NamespacedId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.inner, f)
    }
}

/// The eleven durable object classes described by
/// `docs/pending/workflow-control-plane.md`.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum Prefix {
    /// `wfd_*` — mutable definition identity and ownership metadata.
    WorkflowDefinition,
    /// `wfv_*` — immutable canonical specification.
    WorkflowVersion,
    /// `wft_*` — manual / one-time / cron / webhook / internal trigger.
    WorkflowTrigger,
    /// `wfo_*` — one trigger decision before run admission.
    WorkflowOccurrence,
    /// `wfr_*` — pinned-version lifecycle state.
    WorkflowRun,
    /// `wfn_*` — one materialized node instance (incl. map/loop lineage).
    NodeRun,
    /// `wfa_*` — one leased executor attempt with generation fencing.
    NodeAttempt,
    /// `wfw_*` — durable timer or event wait.
    Wait,
    /// `wfp_*` — principal/role/expiry-bound decision.
    Approval,
    /// `wfe_*` — durable external side-effect intent.
    EffectIntent,
    /// `wf_attn_*` — projection for approvals, input, failures, etc.
    AttentionItem,
    /// `wf_delivery_*` — user-facing delivery record.
    Delivery,
    /// `wf_evt_*` — admitted trigger event (normalized).
    TriggerEvent,
    /// `wfc_*` — immutable claim attached to a node's completion contract.
    Claim,
    /// `wf_ev_*` — hash-bound evidence receipt.
    Evidence,
}

impl Prefix {
    /// Render the prefix as it appears at the start of an id string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Prefix::WorkflowDefinition => "wfd",
            Prefix::WorkflowVersion => "wfv",
            Prefix::WorkflowTrigger => "wft",
            Prefix::WorkflowOccurrence => "wfo",
            Prefix::WorkflowRun => "wfr",
            Prefix::NodeRun => "wfn",
            Prefix::NodeAttempt => "wfa",
            Prefix::Wait => "wfw",
            Prefix::Approval => "wfp",
            Prefix::EffectIntent => "wfe",
            Prefix::AttentionItem => "wf_attn",
            Prefix::Delivery => "wf_delivery",
            Prefix::TriggerEvent => "wf_evt",
            Prefix::Claim => "wfc",
            Prefix::Evidence => "wf_ev",
        }
    }

    /// Parse a prefix from its string form. Returns `None` for unknown.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "wfd" => Prefix::WorkflowDefinition,
            "wfv" => Prefix::WorkflowVersion,
            "wft" => Prefix::WorkflowTrigger,
            "wfo" => Prefix::WorkflowOccurrence,
            "wfr" => Prefix::WorkflowRun,
            "wfn" => Prefix::NodeRun,
            "wfa" => Prefix::NodeAttempt,
            "wfw" => Prefix::Wait,
            "wfp" => Prefix::Approval,
            "wfe" => Prefix::EffectIntent,
            "wf_attn" => Prefix::AttentionItem,
            "wf_delivery" => Prefix::Delivery,
            "wf_evt" => Prefix::TriggerEvent,
            "wfc" => Prefix::Claim,
            "wf_ev" => Prefix::Evidence,
            _ => return None,
        })
    }
}

/// Deterministic, seed-bound ID generator. The same seed and call sequence
/// produce the same id stream across processes, replays, and tests.
///
/// The hash is `sha256(seed || counter_be_u64)`, truncated to 22 base16
/// characters. No `rand`, no `uuid` — reproducibility wins over entropy.
#[derive(Debug, Clone)]
pub struct IdGenerator {
    seed: String,
    counter: u64,
}

impl IdGenerator {
    /// Seed an id generator with `seed`. Two generators with the same seed
    /// produce the same id sequence.
    pub fn seeded(seed: impl Into<String>) -> Self {
        Self {
            seed: seed.into(),
            counter: 0,
        }
    }

    /// Build the next id in `prefix`.
    pub fn next_id(&mut self, prefix: Prefix) -> StableId {
        let raw = self.next_raw();
        StableId::new(prefix, raw)
    }

    /// Build a namespaced id instead of a bare stable id.
    pub fn next_namespaced(&mut self, prefix: Prefix) -> NamespacedId {
        NamespacedId::new(prefix, self.next_raw())
    }

    fn next_raw(&mut self) -> String {
        let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
        hasher.update(self.seed.as_bytes());
        hasher.update(&self.counter.to_be_bytes());
        let digest = hasher.finalize_bytes();
        // 11 bytes = 22 hex chars; collisions remain infeasible at workflow
        // scale and humans can copy-paste without truncation confusion.
        let raw = rightkit_crypto::hex_encode(&digest[..11]);
        self.counter = self.counter.wrapping_add(1);
        raw
    }

    /// Current generation counter. Useful for snapshot tests.
    pub fn counter(&self) -> u64 {
        self.counter
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let mut a = IdGenerator::seeded("unit-test");
        let mut b = IdGenerator::seeded("unit-test");
        for _ in 0..16 {
            assert_eq!(
                a.next_id(Prefix::WorkflowRun),
                b.next_id(Prefix::WorkflowRun)
            );
        }
    }

    #[test]
    fn different_seed_different_sequence() {
        let mut a = IdGenerator::seeded("alpha");
        let mut b = IdGenerator::seeded("beta");
        assert_ne!(
            a.next_id(Prefix::WorkflowRun),
            b.next_id(Prefix::WorkflowRun)
        );
    }

    #[test]
    fn prefix_round_trips() {
        for p in [
            Prefix::WorkflowDefinition,
            Prefix::WorkflowVersion,
            Prefix::WorkflowTrigger,
            Prefix::WorkflowOccurrence,
            Prefix::WorkflowRun,
            Prefix::NodeRun,
            Prefix::NodeAttempt,
            Prefix::Wait,
            Prefix::Approval,
            Prefix::EffectIntent,
            Prefix::AttentionItem,
            Prefix::Delivery,
            Prefix::TriggerEvent,
        ] {
            assert_eq!(Prefix::from_str(p.as_str()), Some(p));
        }
    }

    #[test]
    fn unknown_prefix_does_not_parse() {
        assert_eq!(Prefix::from_str("xyz"), None);
    }

    #[test]
    fn stable_id_split_round_trips() {
        let mut g = IdGenerator::seeded("split-test");
        let id = g.next_id(Prefix::WorkflowVersion);
        let (p, _) = id.split().expect("prefix parses");
        assert_eq!(p, Prefix::WorkflowVersion);
    }

    #[test]
    fn stable_id_normalizes_existing_and_repeated_prefixes() {
        assert_eq!(
            StableId::new(Prefix::WorkflowTrigger, "wft_example").as_str(),
            "wft_example"
        );
        assert_eq!(
            StableId::new(Prefix::WorkflowTrigger, "wft_wft_example").as_str(),
            "wft_example"
        );
    }
}
