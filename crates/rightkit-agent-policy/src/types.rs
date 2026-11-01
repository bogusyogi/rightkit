use serde::{Deserialize, Serialize};

/// Permission mode, least → most permissive (spec §13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionMode {
    /// Read/plan only — no mutation, ever (even with approval).
    Plan,
    /// Mutating tools pause for approval (the default).
    Ask,
    /// File edits auto-apply; shell / destructive still ask.
    AcceptEdits,
    /// Non-destructive mutations auto-apply; destructive / process-control still ask.
    AutoSafe,
    /// Everything allowed — only for an explicit sandbox / disposable worktree.
    Bypass,
}

/// Workspace trust state (spec §13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceTrust {
    /// Read-only until the user approves — effectful actions can never auto-allow, only ask.
    Untrusted,
    /// The user's own workspace — mode governs; writes still ask under `Ask`.
    Trusted,
    /// Mutations route through a worktree/container (clamps to ask until routing is ready).
    Sandboxed,
    /// No tool execution at all.
    Locked,
}

/// The outcome of evaluating a tool call against policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionDecision {
    Allow,
    Ask,
    Deny,
}

/// The result of evaluating a tool call against policy.
/// `policy_source` and `reason` surface in the approval UX / tool-denied result
/// so a block is never opaque.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionVerdict {
    pub decision: PermissionDecision,
    pub reason: String,
    pub policy_source: String,
}

impl PermissionVerdict {
    pub fn new(
        decision: PermissionDecision,
        reason: impl Into<String>,
        policy_source: impl Into<String>,
    ) -> Self {
        Self {
            decision,
            reason: reason.into(),
            policy_source: policy_source.into(),
        }
    }
}

/// S:I4 (DIRECT_PORT_RUST, donor codex `codex-rs/protocol/src/protocol.rs:963`):
/// map Codex's `AskForApproval` approval-policy selector onto the host app's
/// permission-mode ladder. `WorkspaceTrust` already gates untrusted
/// workspaces independently of the requested mode (see `TrustPolicy`), so
/// `UnlessTrusted` and `OnRequest` both resolve to `Ask` here; `Granular`
/// resolves to `Ask` as the conservative default until a category-scoped PDP
/// seam consumes its per-category flags directly. `Never` is the only
/// variant with no ask step, matching Codex's "return failures directly to
/// model" semantics via the host app's `Bypass`.
impl From<rightkit_agent_engine_protocol::AskForApproval> for PermissionMode {
    fn from(ask: rightkit_agent_engine_protocol::AskForApproval) -> Self {
        match ask {
            rightkit_agent_engine_protocol::AskForApproval::Never => PermissionMode::Bypass,
            rightkit_agent_engine_protocol::AskForApproval::OnRequest
            | rightkit_agent_engine_protocol::AskForApproval::UnlessTrusted
            | rightkit_agent_engine_protocol::AskForApproval::Granular(_) => PermissionMode::Ask,
        }
    }
}

/// Parse a permission mode, returning `None` for anything unrecognized.
///
/// Use this wherever a caller stated a mode explicitly — an API request, a CLI
/// flag — so a typo is reported instead of silently becoming `Ask`. Falling
/// back to `Ask` is right for an absent config key and wrong for a misspelled
/// request: the caller gets fail-closed behaviour with nothing explaining why.
///
/// Also accepts Codex's `AskForApproval` wire spellings (`untrusted`,
/// `on-request`, `never`) so a caller ported from Codex's approval-policy
/// vocabulary (S:I4) does not need a translation layer.
pub fn parse_permission_mode_strict(s: &str) -> Option<PermissionMode> {
    match s.trim().to_lowercase().as_str() {
        "plan" => Some(PermissionMode::Plan),
        "ask" => Some(PermissionMode::Ask),
        "accept-edits" | "accept_edits" | "acceptedits" => Some(PermissionMode::AcceptEdits),
        "auto-safe" | "auto_safe" | "autosafe" => Some(PermissionMode::AutoSafe),
        "bypass" => Some(PermissionMode::Bypass),
        // The UI's "code" mode = normal ask-to-mutate
        "code" => Some(PermissionMode::Ask),
        // Codex `AskForApproval` wire spellings (S:I4 direct port).
        "untrusted" => Some(rightkit_agent_engine_protocol::AskForApproval::UnlessTrusted.into()),
        "on-request" | "on_request" | "onrequest" => {
            Some(rightkit_agent_engine_protocol::AskForApproval::OnRequest.into())
        }
        "never" => Some(rightkit_agent_engine_protocol::AskForApproval::Never.into()),
        _ => None,
    }
}

/// Parse a permission mode from a string (mirrors C# `PermissionParse.Mode`).
///
/// Lenient: unknown input becomes `Ask`, the safe default. For explicitly
/// stated modes prefer [`parse_permission_mode_strict`].
pub fn parse_permission_mode(s: Option<&str>) -> PermissionMode {
    parse_permission_mode_strict(s.unwrap_or("")).unwrap_or(PermissionMode::Ask)
}

/// Parse a workspace trust level from a string (mirrors C# `PermissionParse.Trust`).
pub fn parse_workspace_trust(s: Option<&str>) -> WorkspaceTrust {
    match s.unwrap_or("").to_lowercase().as_str() {
        "trusted" => WorkspaceTrust::Trusted,
        "sandboxed" => WorkspaceTrust::Sandboxed,
        "locked" => WorkspaceTrust::Locked,
        _ => WorkspaceTrust::Untrusted,
    }
}

impl PermissionMode {
    /// The user-facing name used by launch flags and slash commands:
    /// `ask`, `accept-edits`, `auto`, `yolo` (plus `plan`).
    pub fn user_name(self) -> &'static str {
        match self {
            PermissionMode::Plan => "plan",
            PermissionMode::Ask => "ask",
            PermissionMode::AcceptEdits => "accept-edits",
            PermissionMode::AutoSafe => "auto",
            PermissionMode::Bypass => "yolo",
        }
    }

    /// Resolve a user-facing name (`ask`, `accept-edits`, `auto`, `yolo`,
    /// `plan`) to a mode. Strict: unknown input is `None`, never `Ask`.
    pub fn from_user_name(name: &str) -> Option<PermissionMode> {
        match name.trim().to_lowercase().as_str() {
            "plan" => Some(PermissionMode::Plan),
            "ask" => Some(PermissionMode::Ask),
            "accept-edits" | "accept_edits" => Some(PermissionMode::AcceptEdits),
            "auto" => Some(PermissionMode::AutoSafe),
            "yolo" => Some(PermissionMode::Bypass),
            _ => None,
        }
    }

    /// Human label for pickers and status text.
    pub fn display_label(self) -> &'static str {
        match self {
            PermissionMode::Plan => "Plan",
            PermissionMode::Ask => "Ask",
            PermissionMode::AcceptEdits => "Accept edits",
            PermissionMode::AutoSafe => "Auto",
            PermissionMode::Bypass => "Yolo",
        }
    }

    /// The spelling the daemon's permission-mode wire (`parse_permission_mode`)
    /// accepts for this mode.
    pub fn wire_name(self) -> &'static str {
        match self {
            PermissionMode::Plan => "plan",
            PermissionMode::Ask => "ask",
            PermissionMode::AcceptEdits => "accept-edits",
            PermissionMode::AutoSafe => "auto-safe",
            PermissionMode::Bypass => "bypass",
        }
    }

    /// Shift+Tab cycles all five policy modes, in the footer's order:
    /// Plan → Ask → Accept edits → Auto → Yolo → Plan. Yolo wraps to the
    /// read-only Plan, never straight back to a mutating mode.
    pub fn next_in_cycle(self) -> PermissionMode {
        match self {
            PermissionMode::Plan => PermissionMode::Ask,
            PermissionMode::Bypass => PermissionMode::Plan,
            PermissionMode::Ask => PermissionMode::AcceptEdits,
            PermissionMode::AcceptEdits => PermissionMode::AutoSafe,
            PermissionMode::AutoSafe => PermissionMode::Bypass,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mode_known_values() {
        assert_eq!(parse_permission_mode(Some("plan")), PermissionMode::Plan);
        assert_eq!(parse_permission_mode(Some("ask")), PermissionMode::Ask);
        assert_eq!(
            parse_permission_mode(Some("accept-edits")),
            PermissionMode::AcceptEdits
        );
        assert_eq!(
            parse_permission_mode(Some("accept_edits")),
            PermissionMode::AcceptEdits
        );
        assert_eq!(
            parse_permission_mode(Some("acceptedits")),
            PermissionMode::AcceptEdits
        );
        assert_eq!(
            parse_permission_mode(Some("auto-safe")),
            PermissionMode::AutoSafe
        );
        assert_eq!(
            parse_permission_mode(Some("bypass")),
            PermissionMode::Bypass
        );
        assert_eq!(parse_permission_mode(Some("code")), PermissionMode::Ask);
        assert_eq!(parse_permission_mode(Some("bogus")), PermissionMode::Ask);
        assert_eq!(parse_permission_mode(None), PermissionMode::Ask);
    }

    #[test]
    fn parse_trust_known_values() {
        assert_eq!(
            parse_workspace_trust(Some("trusted")),
            WorkspaceTrust::Trusted
        );
        assert_eq!(
            parse_workspace_trust(Some("sandboxed")),
            WorkspaceTrust::Sandboxed
        );
        assert_eq!(
            parse_workspace_trust(Some("locked")),
            WorkspaceTrust::Locked
        );
        assert_eq!(
            parse_workspace_trust(Some("bogus")),
            WorkspaceTrust::Untrusted
        );
        assert_eq!(parse_workspace_trust(None), WorkspaceTrust::Untrusted);
    }

    #[test]
    fn strict_parse_accepts_every_documented_spelling() {
        for (input, expected) in [
            ("plan", PermissionMode::Plan),
            ("ask", PermissionMode::Ask),
            ("code", PermissionMode::Ask),
            ("accept-edits", PermissionMode::AcceptEdits),
            ("accept_edits", PermissionMode::AcceptEdits),
            ("auto-safe", PermissionMode::AutoSafe),
            ("AUTO_SAFE", PermissionMode::AutoSafe),
            (" bypass ", PermissionMode::Bypass),
        ] {
            assert_eq!(
                parse_permission_mode_strict(input),
                Some(expected),
                "{input}"
            );
        }
    }

    /// A misspelled mode must be reportable. The lenient parser turns it into
    /// `Ask`, which for an unattended caller means every gated action fails
    /// closed with nothing saying why.
    #[test]
    fn strict_parse_rejects_what_lenient_parse_silently_downgrades() {
        for bad in ["auto_saf", "autosafe!", "yolo", ""] {
            assert_eq!(parse_permission_mode_strict(bad), None, "{bad}");
            assert_eq!(parse_permission_mode(Some(bad)), PermissionMode::Ask);
        }
    }

    /// S:I4: Codex's `AskForApproval` wire spellings must parse through the
    /// same seam a caller already uses for the host app's own mode spellings.
    #[test]
    fn strict_parse_accepts_codex_ask_for_approval_spellings() {
        for (input, expected) in [
            ("untrusted", PermissionMode::Ask),
            ("on-request", PermissionMode::Ask),
            ("on_request", PermissionMode::Ask),
            ("never", PermissionMode::Bypass),
        ] {
            assert_eq!(
                parse_permission_mode_strict(input),
                Some(expected),
                "{input}"
            );
        }
    }

    #[test]
    fn user_facing_names_map_to_modes() {
        for (name, mode) in [
            ("ask", PermissionMode::Ask),
            ("accept-edits", PermissionMode::AcceptEdits),
            ("auto", PermissionMode::AutoSafe),
            ("yolo", PermissionMode::Bypass),
            ("plan", PermissionMode::Plan),
        ] {
            assert_eq!(PermissionMode::from_user_name(name), Some(mode), "{name}");
            assert_eq!(mode.user_name(), name);
            // The daemon wire spelling must round-trip through the lenient parser.
            assert_eq!(parse_permission_mode(Some(mode.wire_name())), mode);
        }
        assert_eq!(PermissionMode::from_user_name("bypass"), None);
        assert_eq!(PermissionMode::Bypass.display_label(), "Yolo");
        assert_eq!(PermissionMode::AcceptEdits.display_label(), "Accept edits");
    }

    #[test]
    fn shift_tab_cycle_order() {
        let mut mode = PermissionMode::Ask;
        let mut seen = Vec::new();
        for _ in 0..5 {
            mode = mode.next_in_cycle();
            seen.push(mode);
        }
        assert_eq!(
            seen,
            vec![
                PermissionMode::AcceptEdits,
                PermissionMode::AutoSafe,
                PermissionMode::Bypass,
                PermissionMode::Plan,
                PermissionMode::Ask,
            ]
        );
        assert_eq!(PermissionMode::Plan.next_in_cycle(), PermissionMode::Ask);
    }

    /// S:I4: every `AskForApproval` variant must convert to a defined
    /// `PermissionMode` with no silent panic or default fallthrough.
    #[test]
    fn ask_for_approval_conversion_covers_every_variant() {
        use rightkit_agent_engine_protocol::{AskForApproval, GranularApprovalConfig};
        assert_eq!(
            PermissionMode::from(AskForApproval::Never),
            PermissionMode::Bypass
        );
        assert_eq!(
            PermissionMode::from(AskForApproval::OnRequest),
            PermissionMode::Ask
        );
        assert_eq!(
            PermissionMode::from(AskForApproval::UnlessTrusted),
            PermissionMode::Ask
        );
        assert_eq!(
            PermissionMode::from(AskForApproval::Granular(GranularApprovalConfig {
                sandbox_approval: true,
                rules: true,
                skill_approval: true,
                request_permissions: true,
                mcp_elicitations: true,
            })),
            PermissionMode::Ask
        );
    }
}
