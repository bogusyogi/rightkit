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
