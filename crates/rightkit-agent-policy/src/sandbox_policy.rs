use serde::{Deserialize, Serialize};

/// Canonical local process-sandbox strength, ordered from least to most restrictive.
///
/// Persisted/UI vocabulary from the first implementation (`none`, `landlock`,
/// `landlock-seccomp`) and execution vocabulary (`workspace-write`, `strict`) both
/// resolve through this type so settings and execution cannot drift.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxTier {
    /// No OS-level restriction. This remains an explicit escape hatch only.
    Disabled,
    /// Filesystem isolation: workspace writes, system/workspace reads.
    WorkspaceWrite,
    /// Filesystem isolation plus syscall/network restrictions where supported.
    /// This matches the settings and diagnostics default.
    #[default]
    Strict,
}

impl SandboxTier {
    /// Parse every tier spelling the host app has persisted or documented.
    pub fn parse_alias(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" | "off" | "disabled" => Some(Self::Disabled),
            "landlock" | "workspace-write" | "workspace_write" | "workspacewrite" => {
                Some(Self::WorkspaceWrite)
            }
            "landlock-seccomp" | "landlock_seccomp" | "strict" | "seccomp" => Some(Self::Strict),
            _ => None,
        }
    }

    /// Parse a configured value, failing closed for invalid non-empty input.
    pub fn parse(value: &str) -> Self {
        Self::parse_alias(value).unwrap_or(Self::Strict)
    }

    /// Stable value used by the existing settings/API surface.
    pub const fn persisted_value(self) -> &'static str {
        match self {
            Self::Disabled => "none",
            Self::WorkspaceWrite => "landlock",
            Self::Strict => "landlock-seccomp",
        }
    }

    pub const fn is_disabled(self) -> bool {
        matches!(self, Self::Disabled)
    }

    /// Whether this tier blocks outbound network access.
    ///
    /// Single source of truth for the question "was that failure ours?", so the
    /// shell can name its own denial instead of letting an EPERM masquerade as
    /// a broken network.
    pub const fn denies_network(self) -> bool {
        matches!(self, Self::Strict)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxPolicySource {
    Default,
    Operator,
    Workspace,
    OperatorAndWorkspace,
}

/// Result of resolving operator/environment and repository-carried settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SandboxPolicyResolution {
    pub effective: SandboxTier,
    pub operator: Option<SandboxTier>,
    pub workspace: Option<SandboxTier>,
    pub source: SandboxPolicySource,
    pub operator_valid: bool,
    pub workspace_valid: bool,
}

/// Resolve the one sandbox authority used by execution and settings consumers.
///
/// The operator/environment value is a minimum restriction. When it is absent,
/// the built-in `Strict` default is the minimum: repository-carried workspace
/// content cannot silently weaken the process sandbox. An operator who wants a
/// weaker floor must opt into it explicitly through the environment; a
/// workspace may then keep or tighten that floor. Invalid non-empty values
/// resolve to `Strict`, preserving fail-closed behavior.
pub fn resolve_sandbox_policy(
    operator_value: Option<&str>,
    workspace_value: Option<&str>,
) -> SandboxPolicyResolution {
    let operator_present = configured(operator_value);
    let workspace_present = configured(workspace_value);
    let operator = operator_present.map(SandboxTier::parse);
    let workspace = workspace_present.map(SandboxTier::parse);
    let operator_valid = operator_present
        .map(|value| SandboxTier::parse_alias(value).is_some())
        .unwrap_or(true);
    let workspace_valid = workspace_present
        .map(|value| SandboxTier::parse_alias(value).is_some())
        .unwrap_or(true);

    let operator_floor = operator.unwrap_or_default();
    let effective = workspace
        .map(|workspace| operator_floor.max(workspace))
        .unwrap_or(operator_floor);
    let source = match (operator.is_some(), workspace.is_some()) {
        (true, true) => SandboxPolicySource::OperatorAndWorkspace,
        (true, false) => SandboxPolicySource::Operator,
        (false, true) => SandboxPolicySource::Workspace,
        (false, false) => SandboxPolicySource::Default,
    };

    SandboxPolicyResolution {
        effective,
        operator,
        workspace,
        source,
        operator_valid,
        workspace_valid,
    }
}

fn configured(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}
