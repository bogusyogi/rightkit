//! Capability descriptors projected from the existing tool registry.
//!
//! Canon (PL-010): "capability descriptors project from the existing tool
//! registry — no second registry." This module therefore owns **no** tool
//! storage. It defines the descriptor shape and the [`CapabilitySource`]
//! projection seam; the daemon implements the seam once over
//! `rightkit_agent_tools::coding_tools::registry::ToolRegistry::capabilities()` (whose
//! `ToolCapability` carries exactly the four projected fields:
//! `name`, `description`, `category`, `read_only`). The wiring note in
//! `docs/canon/daemon-master-canon-atoms.md`
//! carries the exact adapter the orchestrator applies.

use serde::{Deserialize, Serialize};

use super::class::MechanismClass;

/// The authority a requirement needs from its executor. Portable on purpose:
/// Legion plans carry these requirements and the host app binds executors that
/// satisfy them. Values mirror the granularity of
/// `rightkit_agent_policy::CapabilityClass` / Legion `Operation` without importing
/// those crates (orchestration does not depend on them); the daemon seam maps
/// 1:1 when assembling a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityNeed {
    /// Read-only access suffices.
    ReadOnly,
    /// The executor may mutate workspace state.
    Mutate,
    /// The executor may run processes/commands.
    Execute,
    /// The executor may reach the network.
    Network,
    /// The executor must access secrets — never bindable by a deterministic
    /// resolver without an explicit binding.
    Secret,
    /// Only a human may satisfy this requirement.
    HumanApproval,
}

/// One projected capability. Projected — never registered here — from the
/// existing tool registry so the resolver cannot drift from what can actually
/// execute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CapabilityDescriptor {
    pub name: String,
    pub description: String,
    pub category: String,
    pub read_only: bool,
    /// Optional mechanism-class hint contributed at the seam (e.g. the shell
    /// tool hints [`MechanismClass::Process`]). `None` leaves the class to be
    /// derived from authority + signals.
    pub class_hint: Option<MechanismClass>,
}

impl CapabilityDescriptor {
    /// Exact projection of the four `rightkit_agent_tools::coding_tools::registry::ToolCapability`
    /// fields. `class_hint` starts as `None`; the daemon seam may refine it
    /// with `with_class_hint` using `tool_render_class`/policy class metadata.
    pub fn from_registry_projection(
        name: impl Into<String>,
        description: impl Into<String>,
        category: impl Into<String>,
        read_only: bool,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            category: category.into(),
            read_only,
            class_hint: None,
        }
    }

    pub fn with_class_hint(mut self, class_hint: MechanismClass) -> Self {
        self.class_hint = Some(class_hint);
        self
    }

    /// Whether this descriptor can satisfy `need`. Fail-closed: `Secret` and
    /// `HumanApproval` are never satisfied by a plain tool descriptor, and a
    /// mutating need requires a non-read-only descriptor.
    pub fn satisfies(&self, need: AuthorityNeed) -> bool {
        match need {
            AuthorityNeed::ReadOnly => true,
            AuthorityNeed::Mutate | AuthorityNeed::Execute | AuthorityNeed::Network => {
                !self.read_only
            }
            AuthorityNeed::Secret | AuthorityNeed::HumanApproval => false,
        }
    }
}

/// The projection seam. Implemented by the daemon over the existing
/// `ToolRegistry`; the resolver only ever sees descriptors, so no second
/// registry exists by construction.
///
/// `revision` is the stable identity of the projected descriptor set (e.g. a
/// registry generation counter). A decision resolved against revision A is
/// [`super::decision::MechanismOutcome::Stale`] when re-checked against
/// revision B.
pub trait CapabilitySource: Send + Sync {
    fn revision(&self) -> &str;
    fn descriptors(&self) -> Vec<CapabilityDescriptor>;
}

/// In-memory [`CapabilitySource`] for tests and simple seams.
#[derive(Debug, Clone)]
pub struct StaticCapabilitySource {
    revision: String,
    descriptors: Vec<CapabilityDescriptor>,
}

impl StaticCapabilitySource {
    pub fn new(revision: impl Into<String>, descriptors: Vec<CapabilityDescriptor>) -> Self {
        Self {
            revision: revision.into(),
            descriptors,
        }
    }
}

impl CapabilitySource for StaticCapabilitySource {
    fn revision(&self) -> &str {
        &self.revision
    }

    fn descriptors(&self) -> Vec<CapabilityDescriptor> {
        self.descriptors.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_projection_carries_the_four_fields() {
        let descriptor = CapabilityDescriptor::from_registry_projection(
            "read_file",
            "Read a file from the workspace",
            "file",
            true,
        );
        assert_eq!(descriptor.name, "read_file");
        assert_eq!(descriptor.category, "file");
        assert!(descriptor.read_only);
        assert_eq!(descriptor.class_hint, None);
    }

    #[test]
    fn authority_is_fail_closed() {
        let read_only =
            CapabilityDescriptor::from_registry_projection("search", "", "search", true);
        let mutating =
            CapabilityDescriptor::from_registry_projection("write_file", "", "file", false);

        assert!(read_only.satisfies(AuthorityNeed::ReadOnly));
        assert!(!read_only.satisfies(AuthorityNeed::Mutate));
        assert!(!read_only.satisfies(AuthorityNeed::Execute));
        assert!(!read_only.satisfies(AuthorityNeed::Network));
        assert!(!read_only.satisfies(AuthorityNeed::Secret));
        assert!(!read_only.satisfies(AuthorityNeed::HumanApproval));

        assert!(mutating.satisfies(AuthorityNeed::Mutate));
        assert!(mutating.satisfies(AuthorityNeed::Execute));
        assert!(mutating.satisfies(AuthorityNeed::Network));
        assert!(!mutating.satisfies(AuthorityNeed::Secret));
        assert!(!mutating.satisfies(AuthorityNeed::HumanApproval));
    }

    #[test]
    fn static_source_round_trips_revision_and_descriptors() {
        let source = StaticCapabilitySource::new(
            "registry-gen-7",
            vec![CapabilityDescriptor::from_registry_projection(
                "shell", "", "shell", false,
            )],
        );
        assert_eq!(source.revision(), "registry-gen-7");
        assert_eq!(source.descriptors().len(), 1);
    }
}
