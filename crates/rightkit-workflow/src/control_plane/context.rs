//! WP-05 — Context projection (`ContextManifest`).
//!
//! ADR-0021 constraint: every bounded inner executor (native, Codex, Claude,
//! conductor, recipe) operates on a **single, immutable, allowlisted context
//! manifest** that the workflow reducer / journal projection has assembled.
//! The manifest is the *only* object an executor ever sees from outside the
//! node. The properties the manifest must hold are non-negotiable:
//!
//! 1. **Immutable.** Once built, the manifest cannot be mutated by the
//!    executor. The executor can read, but cannot write back into it. All
//!    writes happen through the reducer against the next revision.
//! 2. **Allowlisted.** Every section is one of the closed [`ContextSection`]
//!    variants. Free-form `BTreeMap<String, Value>` projections are
//!    forbidden; the only way to add a new section kind is to add a new
//!    variant here, which forces a compile-time review of every adapter.
//! 3. **No transcripts.** The manifest never contains model traces, tool
//!    call logs, chain-of-thought, or chat history. The executor is
//!    stateless with respect to the run; whatever it needs must be in the
//!    manifest or in its own internal scratch space.
//! 4. **Secret references only.** [`ContextSection::SecretRef`] carries the
//!    name of a secret but never its resolved value. [`ContextSection::Env`]
//!    carries the name of an environment variable but never its value. The
//!    resolver that *fills* those references lives behind the
//!    `secret.read` / `env.read` capability gate (see
//!    [`ScopeValidator`](crate::control_plane::bindings::ScopeValidator)) and is not part
//!    of the manifest itself.
//! 5. **Deterministic hash.** The manifest carries a
//!    [`ContentHash`] computed over its canonical form so the journal can
//!    record exactly which context the executor was given. Two manifests
//!    built from the same inputs at the same revision MUST compare equal
//!    byte-for-byte.
//!
//! ## Crate discipline
//!
//! This module is **pure domain**. It must not depend on `tokio`,
//! `rusqlite`, `axum`, providers, `driver_host`, or Tauri. The manifest is
//! built by the reducer / executor at the daemon boundary (WP-06) and is
//! passed *into* bounded inner executors as a plain value.
//!
#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::control_plane::domain::{
    Capability, CapabilitySet, ContentHash, FailureClass, NodeId, RunId,
};

// ─── Budget ──────────────────────────────────────────────────────────────────

/// Bounded execution budget carried into every node attempt.
///
/// The manifest never *enforces* the budget — that is the executor's job — but
/// every value the executor needs to size its work is recorded here. Limits
/// that are not set on a particular node attempt are `None` (unbounded within
/// the bounds enforced by the compiler / reducer).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBudget {
    /// Wall-clock deadline as Unix milliseconds. Past this instant the
    /// executor MUST transition to [`NodeOutput::Timeout`](crate::control_plane::executors::NodeOutput::Timeout).
    pub deadline_unix_ms: Option<i64>,
    /// Maximum tokens the executor may consume across all provider calls in
    /// this attempt. The executor MUST stop on overflow.
    pub max_tokens: Option<u64>,
    /// Maximum cost in micros of a US dollar. The executor MUST stop on
    /// overflow. Mirrors
    /// `host_budget::BudgetLedger::record_cost` so the units are
    /// compatible with the existing budget ledger.
    pub max_cost_usd_micros: Option<u64>,
    /// Maximum number of inner retries the executor may take before
    /// surfacing failure to the outer workflow. The reducer's bounded retry
    /// policy is the authoritative ceiling; this field is the *advertised*
    /// hint the executor must respect.
    pub max_inner_retries: Option<u32>,
}

impl ContextBudget {
    /// Empty budget — every limit is `None`. The compiler is the source of
    /// truth for limits; this constructor is for tests and for the rare
    /// "unbounded but supervised" node kind.
    pub const fn unbounded() -> Self {
        Self {
            deadline_unix_ms: None,
            max_tokens: None,
            max_cost_usd_micros: None,
            max_inner_retries: None,
        }
    }

    /// True when no limit is set on this attempt. Executors that see an
    /// unbounded manifest must still respect the outer lease TTL.
    pub fn is_unbounded(&self) -> bool {
        self.deadline_unix_ms.is_none()
            && self.max_tokens.is_none()
            && self.max_cost_usd_micros.is_none()
            && self.max_inner_retries.is_none()
    }
}

// ─── Sections ────────────────────────────────────────────────────────────────

/// Discriminator for the iteration context — `Map` vs `Loop`. The
/// [`ContextSection::Iteration`] payload shape differs slightly between the
/// two: `Map` always emits exactly one item per index, `Loop` may emit a
/// sentinel "no-progress" item on the final iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IterationKind {
    /// `Map` node — `index` selects one item from the upstream `items` list.
    Map,
    /// `Loop` node — `index` is the current iteration counter; `item` is
    /// the loop body input (may be `Null` on the first iteration if the
    /// loop body has no prior item).
    Loop,
}

/// Closed set of section kinds the manifest may carry. Adding a new section
/// kind is a deliberate, reviewable event: it forces every adapter that
/// exhaustively matches on this enum to update.
///
/// Each variant is paired with the *narrowest* shape that captures the
/// data; nothing broader. In particular:
/// - [`SecretRef`](Self::SecretRef) / [`Env`](Self::Env) carry only the
///   *name* of the reference. The actual value never enters the manifest.
/// - [`Constant`](Self::Constant) carries the literal value but is identified
///   by its binding hash so the manifest can prove it has not been
///   rewritten.
/// - [`LastError`](Self::LastError) carries a typed failure class plus a
///   *bounded* human-readable message (capped at
///   [`LAST_ERROR_MAX_LEN`] chars; longer messages are truncated at
///   construction).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum ContextSection {
    /// A declared run input. `key` is the declared input name, `value` is
    /// the resolved JSON value.
    RunInput {
        /// Declared input name from `WorkflowDefinition::inputs`.
        key: String,
        /// Resolved JSON value (must already be redacted — see WP-10).
        value: serde_json::Value,
    },
    /// An upstream node's output, referenced by node id. The manifest
    /// records exactly which upstream node produced the value so a
    /// replay can prove it.
    NodeOutput {
        /// Upstream node id (must be in the executor's ancestor set).
        node_id: NodeId,
        /// The upstream node's terminal output JSON (must already be
        /// redacted — see WP-10).
        value: serde_json::Value,
    },
    /// Map / Loop iteration context. Carries the current index plus the
    /// item value the executor should operate on.
    Iteration {
        /// Zero-based iteration index.
        index: u64,
        /// `Map` / `Loop` discriminator.
        kind: IterationKind,
        /// The item the executor should process at `index`. May be
        /// `serde_json::Value::Null` for `Loop`'s first iteration.
        item: serde_json::Value,
    },
    /// Unresolved secret reference. The executor must use the secret
    /// resolver (which sits behind `secret.read`) to resolve the value at
    /// the moment of use; the value MUST never be persisted, logged, or
    /// embedded in the node output.
    SecretRef {
        /// Name of the secret to resolve. Matches
        /// [`BindingKind::SecretRef`](crate::control_plane::bindings::BindingKind::SecretRef).
        name: String,
    },
    /// Unresolved environment variable reference. The executor must use the
    /// env resolver (which sits behind `env.read`) to resolve the value at
    /// the moment of use.
    Env {
        /// Name of the environment variable to resolve. Matches
        /// [`BindingKind::Env`](crate::control_plane::bindings::BindingKind::Env).
        name: String,
    },
    /// A compile-time constant literal. Identified by its binding hash so
    /// the manifest can prove it has not been rewritten between compile and
    /// execution.
    Constant {
        /// Hash of the binding text that produced this constant. The
        /// reducer can re-derive the constant from the binding text and
        /// compare against the stored hash to detect tampering.
        binding_hash: ContentHash,
        /// Resolved literal value. Must already be JSON-clean (no `NaN`,
        /// no `Inf`).
        value: serde_json::Value,
    },
    /// The last failure emitted by the run, surfaced into the executor so
    /// the executor can decide whether to retry, escalate, or branch.
    LastError {
        /// Typed failure class.
        class: FailureClass,
        /// Bounded human-readable message (max
        /// [`LAST_ERROR_MAX_LEN`] chars; longer messages are truncated at
        /// construction).
        message: String,
    },
}

impl ContextSection {
    /// Stable lowercase discriminator suitable for logs and audit records.
    pub const fn kind_label(&self) -> &'static str {
        match self {
            ContextSection::RunInput { .. } => "run_input",
            ContextSection::NodeOutput { .. } => "node_output",
            ContextSection::Iteration { .. } => "iteration",
            ContextSection::SecretRef { .. } => "secret_ref",
            ContextSection::Env { .. } => "env",
            ContextSection::Constant { .. } => "constant",
            ContextSection::LastError { .. } => "last_error",
        }
    }

    /// Borrow the [`serde_json::Value`] payload, when one exists. Returns
    /// `None` for [`ContextSection::SecretRef`] and
    /// [`ContextSection::Env`] (those carry only a name) and for any
    /// future variant that does not embed a value.
    pub fn value(&self) -> Option<&serde_json::Value> {
        match self {
            ContextSection::RunInput { value, .. }
            | ContextSection::NodeOutput { value, .. }
            | ContextSection::Iteration { item: value, .. }
            | ContextSection::Constant { value, .. } => Some(value),
            ContextSection::SecretRef { .. }
            | ContextSection::Env { .. }
            | ContextSection::LastError { .. } => None,
        }
    }
}

/// Maximum length of a [`ContextSection::LastError`] message. Longer
/// messages are truncated at construction so a runaway tool cannot embed
/// an unbounded string into the manifest and bloat the journal.
pub const LAST_ERROR_MAX_LEN: usize = 1024;

/// Maximum number of sections a manifest may carry. The compiler enforces
/// this ceiling when assembling a manifest; an executor that sees a
/// manifest with more sections than this should treat it as a contract
/// violation.
pub const MAX_SECTIONS: usize = 256;

// ─── Provenance ──────────────────────────────────────────────────────────────

/// Per-manifest provenance. The reducer computes a stable
/// [`ContentHash`] over the manifest's canonical form before handing it to
/// the executor; the executor and the journal both record that hash so a
/// post-mortem can prove "this is the exact context the executor saw".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextProvenance {
    /// Manifest schema version. Bumped on backwards-incompatible changes
    /// to [`ContextSection`] variants so older executors can refuse a
    /// manifest they do not understand.
    pub manifest_version: String,
    /// Run id this manifest was built for.
    pub run_id: RunId,
    /// Id of the node this manifest was built for.
    pub node_id: NodeId,
    /// Reducer revision at the moment the manifest was projected. Two
    /// manifests with the same `revision` are guaranteed to have been
    /// built against the same reducer state.
    pub reducer_revision: u64,
    /// Content hash of the manifest's canonical form. See
    /// [`ContentHash::sha256`].
    pub content_hash: ContentHash,
}

// ─── Manifest ────────────────────────────────────────────────────────────────

/// The one and only context object an executor ever sees.
///
/// Construction is gated by [`ContextBuilder`] (private inner type) so the
/// invariants (allowlisted sections, bounded `LastError`, secret-reference
/// only) are enforced at compile time. The manifest itself exposes only
/// read-only accessors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextManifest {
    sections: Vec<ContextSection>,
    budget: ContextBudget,
    capabilities: CapabilitySet,
    provenance: ContextProvenance,
}

impl ContextManifest {
    /// Borrow the section list.
    pub fn sections(&self) -> &[ContextSection] {
        &self.sections
    }

    /// Borrow the budget.
    pub fn budget(&self) -> &ContextBudget {
        &self.budget
    }

    /// Borrow the effective capability set for this node attempt.
    pub fn capabilities(&self) -> &CapabilitySet {
        &self.capabilities
    }

    /// Borrow the provenance.
    pub fn provenance(&self) -> &ContextProvenance {
        &self.provenance
    }

    /// Convenience accessor: the first [`ContextSection::RunInput`] with
    /// matching `key`, if any. Returns `None` when no run input with that
    /// key was projected into the manifest.
    pub fn run_input(&self, key: &str) -> Option<&serde_json::Value> {
        self.sections.iter().find_map(|s| match s {
            ContextSection::RunInput { key: k, value } if k == key => Some(value),
            _ => None,
        })
    }

    /// Convenience accessor: the first [`ContextSection::NodeOutput`] from
    /// `node_id`, if any. Returns `None` when no upstream output from that
    /// node was projected.
    pub fn node_output(&self, node_id: &str) -> Option<&serde_json::Value> {
        self.sections.iter().find_map(|s| match s {
            ContextSection::NodeOutput { node_id: id, value } if id.to_string() == node_id => {
                Some(value)
            }
            _ => None,
        })
    }

    /// Convenience accessor: the [`ContextSection::Iteration`] section, if
    /// any. A manifest has at most one iteration section.
    pub fn iteration(&self) -> Option<&ContextSection> {
        self.sections
            .iter()
            .find(|s| matches!(s, ContextSection::Iteration { .. }))
    }

    /// True when the manifest projects any value (`RunInput`, `NodeOutput`,
    /// `Iteration`, `Constant`). Secret / env references and `LastError`
    /// alone do **not** make the manifest "non-empty" for the purpose of
    /// input projection.
    pub fn has_resolved_values(&self) -> bool {
        self.sections.iter().any(|s| {
            matches!(
                s,
                ContextSection::RunInput { .. }
                    | ContextSection::NodeOutput { .. }
                    | ContextSection::Iteration { .. }
                    | ContextSection::Constant { .. }
            )
        })
    }

    /// Total number of sections. Useful for tests and observability.
    pub fn section_count(&self) -> usize {
        self.sections.len()
    }
}

// ─── Builder ─────────────────────────────────────────────────────────────────

/// Typed failure from constructing a [`ContextManifest`]. The builder is
/// the *only* entry point; every invariant is enforced here, so a manifest
/// that exists at all is guaranteed to be valid.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ContextError {
    /// Manifest already has the maximum number of sections
    /// ([`MAX_SECTIONS`]). The compiler should never produce a manifest
    /// that exceeds this cap; if it does, the projection step is broken.
    #[error("manifest exceeds MAX_SECTIONS ({0})")]
    TooManySections(usize),
    /// Two [`ContextSection::RunInput`] entries have the same `key`.
    /// Duplicates would let an executor read two values for the same
    /// declared input; the compiler must project each declared input
    /// exactly once.
    #[error("duplicate run_input key `{0}`")]
    DuplicateRunInput(String),
    /// Two [`ContextSection::NodeOutput`] entries have the same upstream
    /// `node_id`. Duplicates would let an executor read two values for the
    /// same upstream node.
    #[error("duplicate node_output for node `{0}`")]
    DuplicateNodeOutput(String),
    /// Two [`ContextSection::Iteration`] entries were projected. A manifest
    /// can have at most one iteration section; two would indicate a
    /// projection bug.
    #[error("duplicate iteration section (a manifest may carry at most one)")]
    DuplicateIteration,
    /// [`ContextSection::Constant`] has a `binding_hash` that does not
    /// match the SHA-256 of its value. The compiler computes the binding
    /// hash over the literal text and stores it in the manifest; a
    /// mismatch indicates the constant was rewritten between compile and
    /// dispatch.
    #[error("constant binding_hash does not match its value (expected {expected}, got {got})")]
    ConstantHashMismatch {
        /// Hash the value was supposed to have.
        expected: String,
        /// Hash actually computed from the value.
        got: String,
    },
    /// [`ContextSection::RunInput`] / [`ContextSection::NodeOutput`] /
    /// [`ContextSection::Constant`] carries a `serde_json::Value` that is
    /// not JSON-clean (contains `NaN` or `±Inf`). The manifest should
    /// never carry non-finite literals; the bindings layer rejects them
    /// upstream.
    #[error("non-finite JSON literal in section: {0}")]
    NonFiniteLiteral(String),
}

/// Builder for [`ContextManifest`]. The only entry point for constructing
/// a manifest. Every invariant is enforced as sections / metadata are
/// pushed.
///
/// The builder is single-use: [`build`](Self::build) consumes `self` and
/// returns the immutable manifest.
#[derive(Debug, Clone)]
pub struct ContextBuilder {
    sections: Vec<ContextSection>,
    budget: ContextBudget,
    capabilities: CapabilitySet,
    provenance: ContextProvenance,
}

impl ContextBuilder {
    /// Start a new builder. `provenance.manifest_version` MUST be set by
    /// the caller — the daemon / compiler owns the canonical version
    /// string. The default constructor sets every other field to a safe
    /// constructor seed; callers provide `run_id`, `node_id`,
    /// and `reducer_revision` before [`build`](Self::build).
    pub fn new(manifest_version: impl Into<String>) -> Self {
        Self {
            sections: Vec::new(),
            budget: ContextBudget::unbounded(),
            capabilities: CapabilitySet::empty(),
            provenance: ContextProvenance {
                manifest_version: manifest_version.into(),
                run_id: RunId::new(crate::control_plane::ids::Prefix::WorkflowRun, "unbound"),
                node_id: NodeId::new(crate::control_plane::ids::Prefix::NodeRun, "unbound"),
                reducer_revision: 0,
                content_hash: ContentHash::sha256(b""),
            },
        }
    }

    /// Override the budget.
    #[must_use]
    pub fn with_budget(mut self, budget: ContextBudget) -> Self {
        self.budget = budget;
        self
    }

    /// Override the capability set.
    #[must_use]
    pub fn with_capabilities(mut self, caps: CapabilitySet) -> Self {
        self.capabilities = caps;
        self
    }

    /// Override the provenance's run id / node id / reducer revision. The
    /// `content_hash` is recomputed at [`build`](Self::build) time so the
    /// caller does not need to maintain it.
    #[must_use]
    pub fn with_provenance(
        mut self,
        run_id: RunId,
        node_id: NodeId,
        reducer_revision: u64,
    ) -> Self {
        self.provenance.run_id = run_id;
        self.provenance.node_id = node_id;
        self.provenance.reducer_revision = reducer_revision;
        self
    }

    /// Push a [`ContextSection`]. Each variant enforces its own invariants
    /// (bounded `LastError`, JSON-clean values, no duplicate keys, etc.).
    pub fn push(mut self, section: ContextSection) -> Result<Self, ContextError> {
        // 1. Cardinality cap.
        if self.sections.len() >= MAX_SECTIONS {
            return Err(ContextError::TooManySections(self.sections.len() + 1));
        }

        // 2. Per-variant invariants.
        match &section {
            ContextSection::RunInput { key, value } => {
                ensure_json_clean(value, "run_input")?;
                if self
                    .sections
                    .iter()
                    .any(|s| matches!(s, ContextSection::RunInput { key: k, .. } if k == key))
                {
                    return Err(ContextError::DuplicateRunInput(key.clone()));
                }
            }
            ContextSection::NodeOutput { node_id, value } => {
                ensure_json_clean(value, "node_output")?;
                if self.sections.iter().any(
                    |s| matches!(s, ContextSection::NodeOutput { node_id: n, .. } if n == node_id),
                ) {
                    return Err(ContextError::DuplicateNodeOutput(node_id.to_string()));
                }
            }
            ContextSection::Iteration { item, .. } => {
                ensure_json_clean(item, "iteration")?;
                if self
                    .sections
                    .iter()
                    .any(|s| matches!(s, ContextSection::Iteration { .. }))
                {
                    return Err(ContextError::DuplicateIteration);
                }
            }
            ContextSection::Constant { value, .. } => {
                ensure_json_clean(value, "constant")?;
            }
            ContextSection::SecretRef { .. }
            | ContextSection::Env { .. }
            | ContextSection::LastError { .. } => {
                // No value to validate; SecretRef / Env carry only a name.
                // LastError's `message` is bounded separately below.
            }
        }

        // 3. Bound `LastError.message`.
        let section = match section {
            ContextSection::LastError { class, message } => {
                let bounded = if message.len() > LAST_ERROR_MAX_LEN {
                    message.chars().take(LAST_ERROR_MAX_LEN).collect::<String>()
                } else {
                    message
                };
                ContextSection::LastError {
                    class,
                    message: bounded,
                }
            }
            other => other,
        };

        self.sections.push(section);
        Ok(self)
    }

    /// Consume the builder, validate `Constant` hashes one last time,
    /// compute the canonical content hash, and return the immutable
    /// [`ContextManifest`].
    pub fn build(mut self) -> Result<ContextManifest, ContextError> {
        // 1. Verify every Constant's binding_hash matches its value.
        for section in &self.sections {
            if let ContextSection::Constant {
                binding_hash,
                value,
            } = section
            {
                let computed = hash_constant_value(value);
                if computed != *binding_hash {
                    return Err(ContextError::ConstantHashMismatch {
                        expected: binding_hash.to_string(),
                        got: computed.to_string(),
                    });
                }
            }
        }

        // 2. Compute the canonical content hash over (sections, budget,
        //    capabilities, provenance).
        let canonical = canonical_manifest_bytes(
            &self.sections,
            &self.budget,
            &self.capabilities,
            &self.provenance,
        );
        self.provenance.content_hash = ContentHash::sha256(&canonical);

        Ok(ContextManifest {
            sections: self.sections,
            budget: self.budget,
            capabilities: self.capabilities,
            provenance: self.provenance,
        })
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Walk a JSON value and reject any non-finite numeric (`NaN`, `±Inf`). The
/// manifest should never carry such values; this is the last line of
/// defence behind the bindings layer.
fn ensure_json_clean(value: &serde_json::Value, label: &str) -> Result<(), ContextError> {
    match value {
        serde_json::Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                if !f.is_finite() {
                    return Err(ContextError::NonFiniteLiteral(label.to_string()));
                }
            }
        }
        serde_json::Value::Array(items) => {
            for v in items {
                ensure_json_clean(v, label)?;
            }
        }
        serde_json::Value::Object(obj) => {
            for v in obj.values() {
                ensure_json_clean(v, label)?;
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::String(_) => {}
    }
    Ok(())
}

/// Compute the binding-hash of a constant literal. The compiler must
/// produce the same hash from the binding text; a mismatch at build time
/// means the constant was rewritten between compile and dispatch.
fn hash_constant_value(value: &serde_json::Value) -> ContentHash {
    // Use serde_json's canonical serializer; for the constants the binding
    // produces a deterministic JSON form when fed through `to_string`.
    let canonical = serde_json::to_string(value).unwrap_or_default();
    ContentHash::sha256(canonical.as_bytes())
}

/// Canonical byte representation of a manifest for hashing. The reducer
/// MUST produce the same bytes from the same inputs.
fn canonical_manifest_bytes(
    sections: &[ContextSection],
    budget: &ContextBudget,
    capabilities: &CapabilitySet,
    provenance: &ContextProvenance,
) -> Vec<u8> {
    // We re-use serde_json for serialization; the manifest's `Serialize`
    // impl is the canonical form. Round-tripping through JSON keeps the
    // hash deterministic across platforms.
    let envelope = serde_json::json!({
        "sections": sections,
        "budget": budget,
        "capabilities": capabilities.as_slice().iter().map(|c| c.as_str().to_string()).collect::<Vec<_>>(),
        "manifest_version": provenance.manifest_version,
        "run_id": provenance.run_id.to_string(),
        "node_id": provenance.node_id.to_string(),
        "reducer_revision": provenance.reducer_revision,
    });
    serde_json::to_vec(&envelope).unwrap_or_default()
}

// Silence the unused import warning when `Capability` is only referenced
// transitively in `CapabilitySet`.
#[allow(dead_code)]
const _CAPABILITY_TYPE_ANCHOR: fn() -> Option<Capability> = || None;

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::domain::{Capability, NodeId, RunId};
    use crate::control_plane::ids::Prefix;
    use serde_json::json;

    fn run_id(s: &str) -> RunId {
        RunId::new(Prefix::WorkflowRun, s)
    }

    fn node_id(s: &str) -> NodeId {
        NodeId::new(Prefix::NodeRun, s)
    }

    fn caps(items: &[&str]) -> CapabilitySet {
        CapabilitySet::from_iter(items.iter().map(|s| Capability::new(*s)))
    }

    #[test]
    fn empty_manifest_has_no_sections_and_unbounded_budget() {
        let m = ContextBuilder::new("v1")
            .with_provenance(run_id("r1"), node_id("n1"), 0)
            .build()
            .expect("build");
        assert_eq!(m.section_count(), 0);
        assert!(m.budget().is_unbounded());
        assert!(!m.has_resolved_values());
        assert_eq!(m.provenance().manifest_version, "v1");
        assert_eq!(m.provenance().reducer_revision, 0);
    }

    #[test]
    fn run_input_and_node_output_accessors_return_projected_values() {
        let upstream = node_id("n1").to_string(); // e.g. "nrun:n1"
        let m = ContextBuilder::new("v1")
            .with_provenance(run_id("r1"), node_id("n2"), 7)
            .with_capabilities(caps(&["fs.read"]))
            .push(ContextSection::RunInput {
                key: "user".into(),
                value: json!({"name": "alice"}),
            })
            .expect("push run_input")
            .push(ContextSection::NodeOutput {
                node_id: node_id("n1"),
                value: json!({"answer": 42}),
            })
            .expect("push node_output")
            .build()
            .expect("build");

        assert_eq!(m.run_input("user"), Some(&json!({"name": "alice"})));
        assert!(m.run_input("missing").is_none());
        assert_eq!(m.node_output(&upstream), Some(&json!({"answer": 42})));
        assert!(m.node_output("nrun:other").is_none());
        assert!(m.has_resolved_values());
        assert_eq!(m.provenance().reducer_revision, 7);
        assert!(m.budget().is_unbounded(), "budget still unbounded");
    }

    #[test]
    fn duplicate_run_input_keys_are_rejected() {
        let err = ContextBuilder::new("v1")
            .push(ContextSection::RunInput {
                key: "user".into(),
                value: json!(1),
            })
            .expect("first push ok")
            .push(ContextSection::RunInput {
                key: "user".into(),
                value: json!(2),
            })
            .expect_err("duplicate must fail");
        assert!(matches!(err, ContextError::DuplicateRunInput(ref k) if k == "user"));
    }

    #[test]
    fn duplicate_node_output_ids_are_rejected() {
        let upstream = node_id("n1").to_string(); // e.g. "nrun:n1"
        let err = ContextBuilder::new("v1")
            .push(ContextSection::NodeOutput {
                node_id: node_id("n1"),
                value: json!({"a": 1}),
            })
            .expect("first push ok")
            .push(ContextSection::NodeOutput {
                node_id: node_id("n1"),
                value: json!({"a": 2}),
            })
            .expect_err("duplicate must fail");
        assert!(matches!(err, ContextError::DuplicateNodeOutput(ref n) if n == &upstream));
    }

    #[test]
    fn duplicate_iteration_sections_are_rejected() {
        let a = ContextBuilder::new("v1")
            .push(ContextSection::Iteration {
                index: 0,
                kind: IterationKind::Map,
                item: json!("first"),
            })
            .expect("first iter ok");
        let err = a
            .push(ContextSection::Iteration {
                index: 1,
                kind: IterationKind::Map,
                item: json!("second"),
            })
            .expect_err("second iter must fail");
        assert!(matches!(err, ContextError::DuplicateIteration));
    }

    #[test]
    fn last_error_message_is_truncated_to_max_len() {
        let huge = "x".repeat(LAST_ERROR_MAX_LEN + 50);
        let m = ContextBuilder::new("v1")
            .push(ContextSection::LastError {
                class: FailureClass::TransientProvider,
                message: huge,
            })
            .expect("push last_error")
            .build()
            .expect("build");

        match &m.sections()[0] {
            ContextSection::LastError { message, .. } => {
                assert_eq!(message.len(), LAST_ERROR_MAX_LEN);
                assert!(message.chars().all(|c| c == 'x'));
            }
            other => panic!("expected LastError, got {other:?}"),
        }
    }

    #[test]
    fn constant_hash_mismatch_is_rejected_at_build_time() {
        let true_hash = hash_constant_value(&json!(42));
        // Use a *different* hash to force the mismatch path.
        let wrong_hash = ContentHash::sha256(b"not the value");
        let wrong_hash_for_assert = wrong_hash.clone();
        let err = ContextBuilder::new("v1")
            .push(ContextSection::Constant {
                binding_hash: wrong_hash,
                value: json!(42),
            })
            .expect("push constant ok")
            .build()
            .expect_err("hash mismatch must fail");
        match err {
            ContextError::ConstantHashMismatch { expected, got } => {
                assert_eq!(expected, wrong_hash_for_assert.to_string());
                assert_eq!(got, true_hash.to_string());
            }
            other => panic!("expected ConstantHashMismatch, got {other:?}"),
        }
    }

    #[test]
    fn constant_hash_match_passes_build() {
        let value = json!({"k": [1, 2, 3]});
        let hash = hash_constant_value(&value);
        let m = ContextBuilder::new("v1")
            .push(ContextSection::Constant {
                binding_hash: hash,
                value: value.clone(),
            })
            .expect("push constant ok")
            .build()
            .expect("build");
        assert_eq!(m.sections().len(), 1);
    }

    #[test]
    fn manifest_canonical_hash_is_stable_for_same_inputs() {
        let a = ContextBuilder::new("v1")
            .with_provenance(run_id("r1"), node_id("n1"), 3)
            .with_capabilities(caps(&["fs.read"]))
            .push(ContextSection::RunInput {
                key: "user".into(),
                value: json!({"name": "alice"}),
            })
            .expect("push run_input")
            .build()
            .expect("build a");
        let b = ContextBuilder::new("v1")
            .with_provenance(run_id("r1"), node_id("n1"), 3)
            .with_capabilities(caps(&["fs.read"]))
            .push(ContextSection::RunInput {
                key: "user".into(),
                value: json!({"name": "alice"}),
            })
            .expect("push run_input")
            .build()
            .expect("build b");
        assert_eq!(a.provenance().content_hash, b.provenance().content_hash);
    }

    #[test]
    fn manifest_canonical_hash_changes_when_content_changes() {
        let a = ContextBuilder::new("v1")
            .with_provenance(run_id("r1"), node_id("n1"), 3)
            .push(ContextSection::RunInput {
                key: "user".into(),
                value: json!({"name": "alice"}),
            })
            .expect("push")
            .build()
            .expect("build a");
        let b = ContextBuilder::new("v1")
            .with_provenance(run_id("r1"), node_id("n1"), 3)
            .push(ContextSection::RunInput {
                key: "user".into(),
                value: json!({"name": "bob"}),
            })
            .expect("push")
            .build()
            .expect("build b");
        assert_ne!(a.provenance().content_hash, b.provenance().content_hash);
    }

    #[test]
    fn kind_label_is_stable_per_variant() {
        let cases: &[(&str, ContextSection)] = &[
            (
                "run_input",
                ContextSection::RunInput {
                    key: "k".into(),
                    value: json!(null),
                },
            ),
            (
                "node_output",
                ContextSection::NodeOutput {
                    node_id: node_id("n"),
                    value: json!(null),
                },
            ),
            (
                "iteration",
                ContextSection::Iteration {
                    index: 0,
                    kind: IterationKind::Map,
                    item: json!(null),
                },
            ),
            ("secret_ref", ContextSection::SecretRef { name: "k".into() }),
            ("env", ContextSection::Env { name: "k".into() }),
            (
                "constant",
                ContextSection::Constant {
                    binding_hash: ContentHash::sha256(b""),
                    value: json!(null),
                },
            ),
            (
                "last_error",
                ContextSection::LastError {
                    class: FailureClass::TransientProvider,
                    message: "x".into(),
                },
            ),
        ];
        for (expected, section) in cases {
            assert_eq!(
                section.kind_label(),
                *expected,
                "wrong label for {section:?}"
            );
        }
    }

    #[test]
    fn value_accessor_is_none_for_secret_and_env() {
        let secret = ContextSection::SecretRef {
            name: "api_key".into(),
        };
        let env = ContextSection::Env {
            name: "HOME".into(),
        };
        let last_err = ContextSection::LastError {
            class: FailureClass::TransientProvider,
            message: "boom".into(),
        };
        assert!(secret.value().is_none());
        assert!(env.value().is_none());
        assert!(last_err.value().is_none());
    }

    #[test]
    fn budget_unbounded_default_round_trip() {
        let b = ContextBudget::unbounded();
        assert!(b.is_unbounded());
        let b2 = ContextBudget {
            deadline_unix_ms: Some(1234),
            max_tokens: Some(100),
            max_cost_usd_micros: Some(50),
            max_inner_retries: Some(3),
        };
        assert!(!b2.is_unbounded());
        assert_eq!(b2.deadline_unix_ms, Some(1234));
        assert_eq!(b2.max_inner_retries, Some(3));
    }
}
