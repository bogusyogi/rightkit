//! The mandatory-trigger-rule routing surface.
//!
//! The conductor is handed the agent catalog (see [`crate::catalog`]) plus the
//! [`TRIGGER_RULE`] in its highest-authority prompt slot. It must match the task
//! to exactly one [`Path`] and announce it on one line (`PATH: <x> because ...`).
//! [`parse_announced_path`] reads that line back. The fail-safe — an unannounced
//! or garbled path — is the controller defaulting to [`Path::ArchitectPlan`]
//! (the gated path); that default lives in the controller, not here, but the
//! parser returning `None` is the signal that triggers it.

/// The execution path the conductor routes a task to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Path {
    /// Read/question — answer directly with agentic tools, no gate.
    NonMutating,
    /// A small, localized change — one cheap coder + ponytail + the
    /// deterministic gate. No plan-jury, no end-audit.
    DirectEdit,
    /// Design, repo-wide refactor, or ambiguous — architect plans, the plan is
    /// juried, the user approves, then a fan-out build + end-audit.
    ArchitectPlan,
}

impl Path {
    /// The canonical token the model is asked to emit after `PATH:`.
    pub fn as_token(self) -> &'static str {
        match self {
            Path::NonMutating => "NonMutating",
            Path::DirectEdit => "DirectEdit",
            Path::ArchitectPlan => "ArchitectPlan",
        }
    }
}

/// The mandatory-trigger-rule system text, modeled on Codex's verbatim
/// must-use rule but keyed over the agent catalog's descriptions.
pub const TRIGGER_RULE: &str = "\
You are handed an agent catalog above: each entry is an agent and a description of \
exactly when to use it. You MUST match the user's task to the one agent whose \
description fits it best, and you MUST take the execution path that agent implies. \
This is not a soft preference — if a task matches a description, you are required to \
route to it. Then announce your choice on a single line, exactly in this form:\n\
  PATH: <NonMutating|DirectEdit|ArchitectPlan> because <one short reason>\n\
Routing guide: a pure question or read with no change requested is NonMutating; a \
small, localized change scoped to one or two files is DirectEdit; a new component, a \
repo-wide or cross-cutting change, or anything ambiguous enough to need decomposition \
first is ArchitectPlan. When in doubt between DirectEdit and ArchitectPlan, choose \
ArchitectPlan — the gated path is always the safe default.";

/// Parse the model's announced path from its `PATH: <x> because ...` line.
///
/// Returns `None` when no clean `PATH:` line is present or the token is
/// unrecognized — the controller MUST treat `None` as [`Path::ArchitectPlan`]
/// (fail toward the gated path). Tolerant of casing and `-`/`_`/space in the
/// token (`direct-edit`, `Direct_Edit`, `architect plan` all parse).
pub fn parse_announced_path(model_line: &str) -> Option<Path> {
    // Find the PATH: marker anywhere in the text (the model may prefix prose).
    let lower = model_line.to_ascii_lowercase();
    let idx = lower.find("path:")?;
    let after = &model_line[idx + "path:".len()..];
    // The token is the first run of word-ish chars after the marker, up to
    // "because" / end-of-line / punctuation.
    let token: String = after
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == ' ')
        .collect();
    let norm: String = token
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    // Stop at "because" if it got folded into the token (e.g. no space before it).
    let norm = norm.split("because").next().unwrap_or(&norm);
    match norm {
        "nonmutating" => Some(Path::NonMutating),
        "directedit" => Some(Path::DirectEdit),
        "architectplan" => Some(Path::ArchitectPlan),
        _ => None,
    }
}
