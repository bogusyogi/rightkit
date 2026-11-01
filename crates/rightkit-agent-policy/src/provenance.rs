//! Pure provenance policy for model-requested durable writes.

use crate::{PermissionDecision, PermissionVerdict};
use serde::{Deserialize, Serialize};

/// Highest-risk origin currently able to influence a model-authored tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContentOrigin {
    SessionOrigin,
    ToolDerived,
    WebDerived,
    McpDerived,
}

impl ContentOrigin {
    pub fn label(self) -> &'static str {
        match self {
            Self::SessionOrigin => "session-origin",
            Self::ToolDerived => "tool-derived",
            Self::WebDerived => "web-derived",
            Self::McpDerived => "mcp-derived",
        }
    }

    pub fn is_external(self) -> bool {
        matches!(self, Self::WebDerived | Self::McpDerived)
    }

    fn risk_rank(self) -> u8 {
        match self {
            Self::SessionOrigin => 0,
            Self::ToolDerived => 1,
            Self::WebDerived | Self::McpDerived => 2,
        }
    }
}

/// Structured provenance passed through `ToolContext` to durable-write sinks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentProvenance {
    pub origin: ContentOrigin,
    pub source: String,
}

impl Default for ContentProvenance {
    fn default() -> Self {
        Self::session()
    }
}

impl ContentProvenance {
    pub fn session() -> Self {
        Self {
            origin: ContentOrigin::SessionOrigin,
            source: "session".into(),
        }
    }

    pub fn new(origin: ContentOrigin, source: impl Into<String>) -> Self {
        Self {
            origin,
            source: source.into(),
        }
    }

    /// Browser observations are always external even when the controlled page
    /// is a loopback development server. Bind the exact browser session and
    /// observation epoch so a later durable write cannot shed that origin.
    pub fn browser(browser_session_id: &str, observation_epoch: u64) -> Self {
        Self::new(
            ContentOrigin::WebDerived,
            format!("browser:{browser_session_id}:epoch:{observation_epoch}"),
        )
    }

    /// Conservatively retain the highest-risk source. At equal external risk,
    /// keep the newest source for useful approval/audit attribution.
    pub fn absorb(&mut self, candidate: Self) {
        if candidate.origin.risk_rank() >= self.origin.risk_rank() {
            *self = candidate;
        }
    }
}

/// Durable sink classes. New sinks must opt in explicitly rather than relying
/// on a name classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DurableWriteKind {
    Memory,
    Skill,
    Hook,
    Rule,
    Schedule,
    RepositoryInstruction,
    /// An arbitrary shell can persist memory, policy, hooks, skills, schedules,
    /// or repository instructions without using a typed write tool. Treat the
    /// whole command as a potential durable sink when externally influenced.
    ShellCommand,
}

impl DurableWriteKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Skill => "skill",
            Self::Hook => "hook",
            Self::Rule => "rule",
            Self::Schedule => "schedule",
            Self::RepositoryInstruction => "repository-instruction",
            Self::ShellCommand => "shell-command",
        }
    }
}

pub fn durable_write_requires_approval(provenance: &ContentProvenance) -> bool {
    provenance.origin.is_external()
}

/// Memory writes are stricter than the other durable sinks (contract
/// `context-membrane-memory.md` rules 16 and 45): Membrane stores no memory
/// provenance yet, so a write whose origin is not the user's own session
/// (tool output counts, not only web and MCP content) needs the operator's
/// approval of that exact call.
pub fn memory_write_requires_approval(provenance: &ContentProvenance) -> bool {
    provenance.origin != ContentOrigin::SessionOrigin
}

fn kind_requires_approval(kind: DurableWriteKind, provenance: &ContentProvenance) -> bool {
    match kind {
        DurableWriteKind::Memory => memory_write_requires_approval(provenance),
        _ => durable_write_requires_approval(provenance),
    }
}

/// Apply provenance as a most-restrictive-wins policy layer. Durable writes
/// that need approval ask even when ordinary mode/rules produced Allow
/// (including Bypass).
pub fn enforce_durable_write_provenance(
    base: PermissionVerdict,
    kind: DurableWriteKind,
    provenance: &ContentProvenance,
) -> PermissionVerdict {
    if base.decision == PermissionDecision::Deny || !kind_requires_approval(kind, provenance) {
        return base;
    }

    let source = if provenance.origin.is_external() {
        "provenance:external-durable-write"
    } else {
        "provenance:non-session-memory-write"
    };
    PermissionVerdict::new(
        PermissionDecision::Ask,
        format!(
            "{} durable write is influenced by {} content from '{}'",
            kind.label(),
            provenance.origin.label(),
            provenance.source
        ),
        source,
    )
}

/// Sink-side backstop. The engine's approval flow is not the security boundary:
/// a durable tool called directly still has to present per-call approval.
pub fn durable_write_is_authorized(
    provenance: &ContentProvenance,
    explicitly_approved: bool,
) -> bool {
    !durable_write_requires_approval(provenance) || explicitly_approved
}

/// Sink-side backstop for `memory_save`: the user's own session writes freely,
/// any other origin needs the operator's approval of this exact call.
pub fn memory_write_is_authorized(
    provenance: &ContentProvenance,
    explicitly_approved: bool,
) -> bool {
    !memory_write_requires_approval(provenance) || explicitly_approved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_memory_write_escalates_allow_to_ask() {
        let base = PermissionVerdict::new(
            PermissionDecision::Allow,
            "allowed in bypass",
            "mode:bypass",
        );
        let provenance = ContentProvenance::new(ContentOrigin::WebDerived, "browser_fetch");

        let verdict = enforce_durable_write_provenance(base, DurableWriteKind::Memory, &provenance);

        assert_eq!(verdict.decision, PermissionDecision::Ask);
        assert_eq!(verdict.policy_source, "provenance:external-durable-write");
        assert!(!durable_write_is_authorized(&provenance, false));
        assert!(durable_write_is_authorized(&provenance, true));
    }

    #[test]
    fn hard_denial_and_local_tool_origin_are_preserved_for_other_sinks() {
        let denied = PermissionVerdict::new(PermissionDecision::Deny, "blocked", "deny-glob:write");
        let external = ContentProvenance::new(ContentOrigin::McpDerived, "mcp_call");
        assert_eq!(
            enforce_durable_write_provenance(denied.clone(), DurableWriteKind::Memory, &external),
            denied
        );

        let allowed =
            PermissionVerdict::new(PermissionDecision::Allow, "allowed", "mode:auto-safe");
        let local = ContentProvenance::new(ContentOrigin::ToolDerived, "read_file");
        assert_eq!(
            enforce_durable_write_provenance(allowed.clone(), DurableWriteKind::Skill, &local),
            allowed
        );
    }

    #[test]
    fn memory_write_from_a_non_session_origin_asks_but_the_users_own_session_writes() {
        let allowed =
            PermissionVerdict::new(PermissionDecision::Allow, "allowed", "mode:auto-safe");
        let tool_output = ContentProvenance::new(ContentOrigin::ToolDerived, "read_file");
        let verdict = enforce_durable_write_provenance(
            allowed.clone(),
            DurableWriteKind::Memory,
            &tool_output,
        );
        assert_eq!(verdict.decision, PermissionDecision::Ask);
        assert_eq!(verdict.policy_source, "provenance:non-session-memory-write");
        assert!(!memory_write_is_authorized(&tool_output, false));
        assert!(memory_write_is_authorized(&tool_output, true));

        let own_session = ContentProvenance::session();
        assert_eq!(
            enforce_durable_write_provenance(
                allowed.clone(),
                DurableWriteKind::Memory,
                &own_session
            ),
            allowed
        );
        assert!(memory_write_is_authorized(&own_session, false));
    }

    #[test]
    fn browser_observation_is_external_and_epoch_bound() {
        let provenance = ContentProvenance::browser("bs-7", 12);
        assert_eq!(provenance.origin, ContentOrigin::WebDerived);
        assert_eq!(provenance.source, "browser:bs-7:epoch:12");
        assert!(durable_write_requires_approval(&provenance));
    }
}
