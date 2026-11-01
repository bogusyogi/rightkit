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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{PermissionMode, WorkspaceTrust};

    // Mirrors C# TrustPolicyTests [Theory].
    #[test]
    fn requires_isolation_only_for_unsandboxed_bypass() {
        // Bypass + Untrusted → true (would mutate real tree unattended)
        assert!(TrustPolicy::requires_isolation(
            PermissionMode::Bypass,
            WorkspaceTrust::Untrusted
        ));
        // Bypass + Trusted → true (still unsandboxed)
        assert!(TrustPolicy::requires_isolation(
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted
        ));
        // Bypass + Sandboxed → false (already isolated)
        assert!(!TrustPolicy::requires_isolation(
            PermissionMode::Bypass,
            WorkspaceTrust::Sandboxed
        ));
        // Ask + Untrusted → false (supervised, not autonomous)
        assert!(!TrustPolicy::requires_isolation(
            PermissionMode::Ask,
            WorkspaceTrust::Untrusted
        ));
        // AutoSafe + Trusted → false
        assert!(!TrustPolicy::requires_isolation(
            PermissionMode::AutoSafe,
            WorkspaceTrust::Trusted
        ));
    }

    #[test]
    fn bypass_untrusted_forces_worktree() {
        // FM-034: Bypass on non-sandboxed workspace must force worktree isolation
        let requires =
            TrustPolicy::requires_isolation(PermissionMode::Bypass, WorkspaceTrust::Untrusted);
        assert!(
            requires,
            "Bypass+Untrusted must require isolation (forces worktree)"
        );
        // Simulate routing decision: true forces worktree
        let forced_worktree = if requires {
            Some("/tmp/worktree")
        } else {
            None
        };
        assert!(forced_worktree.is_some(), "worktree must be forced");
    }

    #[test]
    fn bypass_sandboxed_allows_in_place() {
        // FM-034: Bypass on already-sandboxed workspace does not force worktree
        let requires =
            TrustPolicy::requires_isolation(PermissionMode::Bypass, WorkspaceTrust::Sandboxed);
        assert!(
            !requires,
            "Bypass+Sandboxed must not require isolation (already isolated)"
        );
        let forced_worktree = if requires {
            Some("/tmp/worktree")
        } else {
            None
        };
        assert!(
            forced_worktree.is_none(),
            "in-place allowed when already sandboxed"
        );
    }
}
