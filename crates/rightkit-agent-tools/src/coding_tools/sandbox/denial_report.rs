// Thin the host app adapter over `rightkit_sandbox::report_denial`: classification,
// denied-path extraction, protected-secret refusal and the narrowest requested
// grant are RightKit's. What stays here is the host app's own model: the tier the
// command ran under, the `SandboxDenial` shape the engine's admission authority
// consumes, and the `[the host app sandbox denial: ...]` rendering.
//! Typed, human-readable sandbox denial reports and the grants that can lift
//! them.
//!
//! A confined command that fails because the sandbox refused it used to come
//! back as a bare OS error. This module turns the command's own output into a
//! [`SandboxDenial`] that names what was blocked, says it was the sandbox, and
//! carries the narrowest [`SandboxGrant`] that would let a re-run succeed (or
//! `None` when nothing may lift it: protected secrets).

pub use rightkit_sandbox::{looks_like_blocked_network, Grant as SandboxGrant};
use rightkit_sandbox::{DenialCategory, DenialSource};
use std::path::PathBuf;

/// What the sandbox blocked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxDenialKind {
    Network,
    Filesystem {
        path: PathBuf,
        /// On the protected-secret list: never grantable.
        protected_secret: bool,
    },
}

/// A classified, reportable sandbox denial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxDenial {
    pub kind: SandboxDenialKind,
    /// Shared vocabulary with the policy crate's escalation ladder
    /// (`"network"`, `"filesystem"`).
    pub category: DenialCategory,
    /// Tier the command ran under when it was blocked.
    pub tier: rightkit_agent_policy::SandboxTier,
    /// Stable machine identifier, e.g. `sandbox-denial:network`.
    pub rule_id: String,
    /// Human-readable, model- and operator-facing explanation.
    pub message: String,
    /// The narrowest grant that lifts the denial; `None` = hard stop.
    pub requested_grant: Option<SandboxGrant>,
}

impl SandboxDenial {
    /// The text block appended to the tool result.
    pub fn render(&self) -> String {
        format!("[Agent sandbox denial: {}] {}", self.rule_id, self.message)
    }
}

/// How the command was confined when it ran.
#[derive(Debug, Clone)]
pub struct ConfinementInfo {
    pub source: DenialSource,
    pub tier: rightkit_agent_policy::SandboxTier,
    /// Whether this run was allowed outbound network.
    pub network_allowed: bool,
    pub workspace_root: PathBuf,
}

/// Classify a confined command's output into a reportable denial.
///
/// Returns `None` when the failure is not recognisably the sandbox's doing:
/// an unrecognised failure is never labelled a sandbox denial (it may be a
/// real bug), and a failure on a path inside the workspace is not a grant
/// problem.
pub fn report_confined_denial(
    output: &str,
    command: &str,
    exit_ok: bool,
    info: &ConfinementInfo,
) -> Option<SandboxDenial> {
    let report = rightkit_sandbox::report_denial(
        output,
        command,
        exit_ok,
        &rightkit_sandbox::ConfinementInfo {
            source: info.source,
            network_allowed: info.network_allowed,
            workspace_root: info.workspace_root.clone(),
        },
    )?;
    let rule_id = report.rule_id().to_string();
    let kind = match report.category {
        DenialCategory::Network => SandboxDenialKind::Network,
        _ => SandboxDenialKind::Filesystem {
            path: report.denied_path.clone().unwrap_or_default(),
            protected_secret: report.protected_secret,
        },
    };
    Some(SandboxDenial {
        kind,
        category: report.category,
        tier: info.tier,
        rule_id,
        message: report.message,
        requested_grant: report.requested_grant,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_http_timeout_requests_only_a_command_network_grant() {
        let output = "Invoke-WebRequest : The operation has timed out.\n\
                      FullyQualifiedErrorId : WebCmdletWebResponseException,\
                      Microsoft.PowerShell.Commands.InvokeWebRequestCommand";
        let command = "Invoke-WebRequest -TimeoutSec 5 'http://127.0.0.1:12345/'";
        let mut info = ConfinementInfo {
            source: DenialSource::WindowsAppContainer,
            tier: rightkit_agent_policy::SandboxTier::Strict,
            network_allowed: false,
            workspace_root: PathBuf::from("workspace"),
        };
        let denial = report_confined_denial(output, command, false, &info)
            .expect("network-disabled AppContainer HTTP timeout is a denial");
        assert_eq!(denial.kind, SandboxDenialKind::Network);
        assert_eq!(denial.requested_grant, Some(SandboxGrant::network()));
        assert!(report_confined_denial(output, command, true, &info).is_none());
        assert!(
            report_confined_denial("The operation has timed out.", command, false, &info).is_none()
        );
        info.network_allowed = true;
        assert!(report_confined_denial(output, command, false, &info).is_none());
        info.network_allowed = false;
        info.source = DenialSource::MacosSeatbelt;
        assert!(report_confined_denial(output, command, false, &info).is_none());
    }
}
