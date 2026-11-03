//! Workspace trust gate (ENG-89, M1).
//!
//! First-open explicit user trust decision before any agent tool runs in a
//! workspace. This is distinct from workspace **containment** (the
//! git-worktree / sandbox routing owned by [`crate::trust::TrustPolicy`] and
//! the engine's containment machinery): containment decides *where* a
//! mutation lands once a tool is already permitted to run; this gate decides
//! *whether any agent tool may run at all* for a workspace whose trust
//! posture has never been explicitly recorded, or whose prior trust grant
//! was revoked.
//!
//! `rightkit-agent-policy` performs no I/O (see crate docs), so this module holds
//! decisions in an injectable, in-memory [`WorkspaceTrustGate`]. The host
//! (engine / config layer) is responsible for loading persisted per-workspace
//! decisions into the gate at startup and for writing back changes made
//! through [`WorkspaceTrustGate::record_decision`] — the gate itself never
//! touches disk.

use std::collections::HashMap;

/// The explicit trust decision recorded for a workspace.
///
/// `Unknown` covers both "never opened before" and "no decision recorded
/// yet" — both require the same first-open prompt. `Revoked` is distinct
/// from `Unknown` only for audit/UX purposes (e.g. "you previously
/// untrusted this workspace"); both block tool execution identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustGateState {
    /// No explicit decision on record — first open, or never decided.
    Unknown,
    /// The user explicitly trusted this workspace.
    Trusted,
    /// The user explicitly declined to trust this workspace.
    Denied,
    /// A prior `Trusted` decision was later revoked.
    Revoked,
}

impl TrustGateState {
    /// Whether this state permits agent tools to run (subject to all other
    /// policy checks — this is a prerequisite gate, not a full verdict).
    pub fn permits_tools(self) -> bool {
        matches!(self, TrustGateState::Trusted)
    }
}

/// Outcome of a gate check, shaped to compose with `PermissionDecision`
/// (`crate::types::PermissionDecision`) at the PDP call site without this
/// crate-internal module depending on `pdp`'s request types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateOutcome {
    /// `true` when the workspace is trusted and tool execution may proceed
    /// to the remaining policy checks.
    pub allowed: bool,
    /// Machine-stable identifier for rule attribution in the decision log,
    /// e.g. `"workspace-trust-gate:unknown"`.
    pub rule_id: String,
    /// Human-readable reason surfaced in the pending/deny verdict.
    pub reason: String,
}

/// First-open workspace trust gate.
///
/// Holds an in-memory map of workspace identity → trust decision. Construct
/// empty and seed it from persisted state, or use [`WorkspaceTrustGate::seeded`].
#[derive(Debug, Clone, Default)]
pub struct WorkspaceTrustGate {
    decisions: HashMap<String, TrustGateState>,
}

impl WorkspaceTrustGate {
    /// Create an empty gate — every workspace starts `Unknown`.
    pub fn new() -> Self {
        Self {
            decisions: HashMap::new(),
        }
    }

    /// Create a gate pre-populated from persisted `(workspace_id, state)`
    /// pairs (e.g. loaded by the config crate from disk).
    pub fn seeded(entries: impl IntoIterator<Item = (String, TrustGateState)>) -> Self {
        Self {
            decisions: entries.into_iter().collect(),
        }
    }

    /// Look up the current trust state for a workspace, defaulting to
    /// `Unknown` when no decision has been recorded.
    pub fn state_for(&self, workspace_id: &str) -> TrustGateState {
        self.decisions
            .get(workspace_id)
            .copied()
            .unwrap_or(TrustGateState::Unknown)
    }

    /// Record an explicit user decision for a workspace. The caller is
    /// responsible for persisting this beyond process lifetime.
    pub fn record_decision(&mut self, workspace_id: impl Into<String>, state: TrustGateState) {
        self.decisions.insert(workspace_id.into(), state);
    }

    /// Revoke a previously trusted workspace, re-blocking tool execution
    /// until the user makes a new explicit decision.
    pub fn revoke(&mut self, workspace_id: &str) {
        self.decisions
            .insert(workspace_id.to_string(), TrustGateState::Revoked);
    }

    /// Evaluate whether an agent tool may run in `workspace_id`.
    ///
    /// This must be consulted prior to, and separately from, containment
    /// checks: containment answers "how is this mutation isolated", this
    /// answers "is any agent tool permitted here at all".
    pub fn check(&self, workspace_id: &str) -> GateOutcome {
        let state = self.state_for(workspace_id);
        match state {
            TrustGateState::Trusted => GateOutcome {
                allowed: true,
                rule_id: "workspace-trust-gate:trusted".to_string(),
                reason: "workspace explicitly trusted".to_string(),
            },
            TrustGateState::Unknown => GateOutcome {
                allowed: false,
                rule_id: "workspace-trust-gate:unknown".to_string(),
                reason: "workspace opened for the first time; explicit trust decision required"
                    .to_string(),
            },
            TrustGateState::Denied => GateOutcome {
                allowed: false,
                rule_id: "workspace-trust-gate:denied".to_string(),
                reason: "user declined to trust this workspace".to_string(),
            },
            TrustGateState::Revoked => GateOutcome {
                allowed: false,
                rule_id: "workspace-trust-gate:revoked".to_string(),
                reason: "trust for this workspace was revoked; explicit re-decision required"
                    .to_string(),
            },
        }
    }
}
