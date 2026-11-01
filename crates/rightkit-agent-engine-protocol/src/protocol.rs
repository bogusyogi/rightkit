//! Approval policy contracts ported from Codex's portable protocol.
//!
//! DIRECT_PORT_RUST (atom `S:I4`): ported verbatim from codex
//! `codex-rs/protocol/src/protocol.rs:963` (`AskForApproval`,
//! `SandboxPolicy`'s network-access leg re-cast here as `NetworkAccess`) plus
//! the granular per-category approval controls added alongside it. See
//! `docs/canon/daemon-master-canon-atoms.md` (donor-verification correction,
//! 2026-08-29: Waveloom/Go and Pi/TypeScript are not valid `E→A` sources for
//! this atom; Codex is the sole donor) for the exact donor revision and
//! NOTICE entry. No behavioral changes were made at this seam beyond the
//! module path (`codex_protocol::protocol` -> `rightkit_agent_engine_protocol::protocol`)
//! and wiring `AskForApproval` onto the host app's own `PermissionMode` ladder
//! (see `host_policy::types::parse_permission_mode_strict`) rather than
//! introducing a second approval-mode enum at the seam.

use serde::{Deserialize, Serialize};

/// Determines when an operator is consulted before command execution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AskForApproval {
    /// Require approval for untrusted projects.
    #[serde(rename = "untrusted")]
    UnlessTrusted,
    /// Ask when policy/model requests it.
    #[default]
    OnRequest,
    /// Apply per-category controls.
    Granular(GranularApprovalConfig),
    /// Never ask; return failures directly to model.
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GranularApprovalConfig {
    pub sandbox_approval: bool,
    pub rules: bool,
    #[serde(default)]
    pub skill_approval: bool,
    #[serde(default)]
    pub request_permissions: bool,
    pub mcp_elicitations: bool,
}

impl GranularApprovalConfig {
    pub const fn allows_sandbox_approval(self) -> bool {
        self.sandbox_approval
    }
    pub const fn allows_rules_approval(self) -> bool {
        self.rules
    }
    pub const fn allows_skill_approval(self) -> bool {
        self.skill_approval
    }
    pub const fn allows_request_permissions(self) -> bool {
        self.request_permissions
    }
    pub const fn allows_mcp_elicitations(self) -> bool {
        self.mcp_elicitations
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkAccess {
    #[default]
    Restricted,
    Enabled,
}

impl NetworkAccess {
    pub const fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// S:I4: `AskForApproval` must keep Codex's exact wire tags -- the
    /// `#[serde(rename = "untrusted")]` on `UnlessTrusted` is a deliberate
    /// deviation from the field's own kebab-case name and must not regress
    /// to the derived `unless-trusted` spelling.
    #[test]
    fn ask_for_approval_keeps_codex_wire_tags() {
        assert_eq!(
            serde_json::to_value(AskForApproval::UnlessTrusted).unwrap(),
            serde_json::json!("untrusted")
        );
        assert_eq!(
            serde_json::to_value(AskForApproval::OnRequest).unwrap(),
            serde_json::json!("on-request")
        );
        assert_eq!(
            serde_json::to_value(AskForApproval::Never).unwrap(),
            serde_json::json!("never")
        );
        assert_eq!(
            serde_json::from_value::<AskForApproval>(serde_json::json!("untrusted")).unwrap(),
            AskForApproval::UnlessTrusted
        );
    }

    #[test]
    fn ask_for_approval_defaults_to_on_request() {
        assert_eq!(AskForApproval::default(), AskForApproval::OnRequest);
    }

    #[test]
    fn granular_approval_config_round_trips() {
        let config = GranularApprovalConfig {
            sandbox_approval: true,
            rules: false,
            skill_approval: true,
            request_permissions: false,
            mcp_elicitations: true,
        };
        let ask = AskForApproval::Granular(config);
        let value = serde_json::to_value(ask).unwrap();
        let back: AskForApproval = serde_json::from_value(value).unwrap();
        assert_eq!(back, ask);
        assert!(config.allows_sandbox_approval());
        assert!(!config.allows_rules_approval());
        assert!(config.allows_skill_approval());
        assert!(!config.allows_request_permissions());
        assert!(config.allows_mcp_elicitations());
    }

    #[test]
    fn network_access_keeps_kebab_case_wire_tags() {
        assert_eq!(
            serde_json::to_value(NetworkAccess::Restricted).unwrap(),
            serde_json::json!("restricted")
        );
        assert_eq!(
            serde_json::to_value(NetworkAccess::Enabled).unwrap(),
            serde_json::json!("enabled")
        );
        assert!(!NetworkAccess::Restricted.is_enabled());
        assert!(NetworkAccess::Enabled.is_enabled());
        assert_eq!(NetworkAccess::default(), NetworkAccess::Restricted);
    }
}
