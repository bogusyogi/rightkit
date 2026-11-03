use crate::types::{PermissionMode, WorkspaceTrust};

/// Trust/sandbox policy helpers (H6, spec §13).
///
/// This module governs **containment routing**: given a workspace's trust
/// posture, whether an autonomous run must be isolated into a worktree/
/// sandbox. It assumes tool execution in the workspace is already permitted.
///
/// It is distinct from the **first-open trust gate** in
/// [`crate::workspace_trust_gate`] (ENG-89), which answers a prior question
/// — whether any agent tool may run in this workspace at all, before an
/// explicit user trust decision has been recorded. Do not fold the two
/// together: containment decides *how* a permitted mutation is isolated;
/// the gate decides *whether* execution is permitted in the first place.
pub struct TrustPolicy;

impl TrustPolicy {
    /// Autonomous / unattended runs must be isolated.
    ///
    /// Returns `true` when the mode is [`PermissionMode::Bypass`] (the unsupervised
    /// "run everything" mode) but the workspace is not [`WorkspaceTrust::Sandboxed`]
    /// — i.e. the agent would mutate the real working tree unattended. The session
    /// routes through a git worktree when sandboxed; this surfaces the requirement
    /// (full auto-enforcement arrives with goal mode, post-S6).
    pub fn requires_isolation(mode: PermissionMode, trust: WorkspaceTrust) -> bool {
        mode == PermissionMode::Bypass && trust != WorkspaceTrust::Sandboxed
    }
}
