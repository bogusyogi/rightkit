use crate::coding_tools::context::ToolContext;
use rightkit_agent_policy::DurableWriteKind;
use std::path::{Component, Path};

/// Classify repository files whose contents can change future agent behavior.
/// Matching is case-insensitive for Windows and normalizes lexical `..` parts.
pub(crate) fn repository_policy_kind(path: &Path) -> Option<DurableWriteKind> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(part) => parts.push(part.to_string_lossy().to_ascii_lowercase()),
        }
    }

    if let Some(kind) = state_root_policy_kind(&parts) {
        return Some(kind);
    }

    let file = parts.last()?.as_str();
    if matches!(file, "agents.md" | "claude.md") {
        return Some(DurableWriteKind::RepositoryInstruction);
    }
    if file == "right.md" {
        return Some(DurableWriteKind::RepositoryInstruction);
    }

    for pair in parts.windows(2) {
        match (pair[0].as_str(), pair[1].as_str()) {
            (".rightkit", "hooks") => return Some(DurableWriteKind::Hook),
            (".rightkit", "skills") | (".rightkit", "agents") | (".claude", "skills") => {
                return Some(DurableWriteKind::Skill)
            }
            (".rightkit", "rules") | (".claude", "rules") => return Some(DurableWriteKind::Rule),
            (".rightkit", "instructions") | (".claude", "instructions") => {
                return Some(DurableWriteKind::RepositoryInstruction)
            }
            _ => {}
        }
    }

    if parts.len() >= 2 && parts[parts.len() - 2] == ".rightkit" && file == "policy.toml" {
        return Some(DurableWriteKind::Rule);
    }
    None
}

/// User-level behaviour files live directly under the state root (the rightkit-daemon AppRoot,
/// `<platform data dir>/Orthic Labs/the host app`), which no longer carries a `<state-dir>` path
/// component. Classify the same names there as the workspace `<state-dir>/` rules do. `parts` is
/// the lower-cased, `..`-resolved component list built by [`repository_policy_kind`].
fn state_root_policy_kind(parts: &[String]) -> Option<DurableWriteKind> {
    let root = rightkit_app_settings::agent_config::state_root()?;
    let mut root_parts: Vec<String> = Vec::new();
    for component in root.components() {
        match component {
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
            Component::ParentDir => {
                root_parts.pop();
            }
            Component::Normal(part) => root_parts.push(part.to_string_lossy().to_ascii_lowercase()),
        }
    }
    let rest = parts.strip_prefix(root_parts.as_slice())?;
    match rest {
        [dir, ..] if dir == "hooks" => Some(DurableWriteKind::Hook),
        [dir, ..] if dir == "skills" || dir == "agents" => Some(DurableWriteKind::Skill),
        [dir, ..] if dir == "rules" => Some(DurableWriteKind::Rule),
        [dir, ..] if dir == "instructions" => Some(DurableWriteKind::RepositoryInstruction),
        [file] if file == "policy.toml" => Some(DurableWriteKind::Rule),
        _ => None,
    }
}

pub(crate) fn path_arg_kind(args: &serde_json::Value) -> Option<DurableWriteKind> {
    args.get("path")
        .and_then(serde_json::Value::as_str)
        .and_then(|path| repository_policy_kind(Path::new(path)))
}

pub(crate) fn patch_arg_kind(args: &serde_json::Value) -> Option<DurableWriteKind> {
    args.get("patch")
        .and_then(serde_json::Value::as_str)?
        .lines()
        .filter_map(|line| line.strip_prefix("*** "))
        .find_map(|path| repository_policy_kind(Path::new(path)))
}

pub(crate) fn require_path_authorization(path: &Path, ctx: &ToolContext) -> Result<(), String> {
    let Some(kind) = repository_policy_kind(path) else {
        return Ok(());
    };
    if rightkit_agent_policy::durable_write_is_authorized(
        &ctx.content_provenance,
        ctx.durable_write_approved,
    ) {
        return Ok(());
    }
    Err(format!(
        "{} durable write influenced by {} content from '{}' requires explicit approval",
        kind.label(),
        ctx.content_provenance.origin.label(),
        ctx.content_provenance.source
    ))
}
