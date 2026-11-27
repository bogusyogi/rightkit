//! What an approval request tells the person about the call: what it changes, whether it leaves
//! the sandbox, and whether it can be undone.
//!
//! [`approval_effects`] is a pure function of facts the engine already holds when it raises the
//! request: the tool name and capability class, the call arguments, the workspace root and the
//! sandbox tier the call would run under. It reads no files and runs no commands. A fact the
//! engine cannot know is reported as `unknown` with a reason, never guessed.
//!
//! Paths come from the arguments the call names (`path`, `file_path`, `notebook_path`,
//! `destination`, git `paths`, and the file headers of a patch). A shell command is not parsed:
//! the files it changes are not known in advance.

use rightkit_agent_engine_protocol::{ApprovalEffects, ApprovalFact, ApprovalFactValue};
use rightkit_agent_policy::{CapabilityClass, SandboxTier};
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

/// The sandbox the call would run under, as far as the engine knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxFact {
    /// No operating-system sandbox for this workspace.
    Disabled,
    /// Files are confined to the workspace; the network is open.
    FilesConfined,
    /// Files are confined and the network is off.
    Strict,
}

impl From<SandboxTier> for SandboxFact {
    fn from(tier: SandboxTier) -> Self {
        match tier {
            SandboxTier::Disabled => SandboxFact::Disabled,
            SandboxTier::WorkspaceWrite => SandboxFact::FilesConfined,
            SandboxTier::Strict => SandboxFact::Strict,
        }
    }
}

/// Everything [`approval_effects`] reads.
pub(crate) struct EffectInputs<'a> {
    pub tool_name: &'a str,
    pub capability: CapabilityClass,
    pub args: &'a Value,
    pub workspace_root: &'a Path,
    /// The sandbox tier the call would run under; `None` when the engine cannot tell (for
    /// example a build without a process sandbox).
    pub sandbox: Option<SandboxFact>,
    /// The call carries a one-time sandbox exception (a retry grant).
    pub sandbox_grant: bool,
}

/// File tools whose edits the engine snapshots before the call runs (`Tool::write_targets`), so
/// rewind restores them.
const SNAPSHOT_TOOLS: &[&str] = &[
    "edit_file",
    "write_file",
    "multi_edit",
    "apply_patch",
    "notebook",
    "undo_edit",
];

fn yes(reason: &str) -> ApprovalFact {
    ApprovalFact::new(ApprovalFactValue::Yes, reason)
}
fn no(reason: &str) -> ApprovalFact {
    ApprovalFact::new(ApprovalFactValue::No, reason)
}
fn unknown(reason: &str) -> ApprovalFact {
    ApprovalFact::new(ApprovalFactValue::Unknown, reason)
}

/// Work out the three facts for one call. See the module comment for what each is based on.
pub(crate) fn approval_effects(input: &EffectInputs<'_>) -> ApprovalEffects {
    let mut effects = match input.capability {
        CapabilityClass::WriteFile | CapabilityClass::Destructive => file_effects(input),
        CapabilityClass::Shell => shell_effects(input),
        CapabilityClass::Network
        | CapabilityClass::Browser
        | CapabilityClass::Mcp
        | CapabilityClass::Media
        | CapabilityClass::Secrets => outside_effects(input),
        CapabilityClass::GitWrite => git_effects(input),
        CapabilityClass::ProcessControl => ApprovalEffects {
            changes: Vec::new(),
            changes_summary: "Stops or controls a running process".into(),
            leaves_sandbox: no("Only affects a process this session started"),
            can_be_undone: no("A stopped process stays stopped"),
        },
        CapabilityClass::ReadFile => read_effects(input),
        CapabilityClass::ArtifactTrust | CapabilityClass::AddContext | CapabilityClass::Unknown => {
            ApprovalEffects {
                changes: Vec::new(),
                changes_summary: "What this does is not known in advance".into(),
                leaves_sandbox: unknown("The engine cannot tell where this runs"),
                can_be_undone: unknown("The engine cannot tell whether this can be undone"),
            }
        }
    };
    if input.sandbox_grant && input.capability != CapabilityClass::ReadFile {
        effects.leaves_sandbox = yes("Needs a one-time exception to the sandbox");
    }
    effects
}

// ---------------------------------------------------------------------------
// File tools

fn file_effects(input: &EffectInputs<'_>) -> ApprovalEffects {
    let declared = declared_paths(input.tool_name, input.args);
    let mut inside: Vec<String> = Vec::new();
    let mut outside: Vec<String> = Vec::new();
    for raw in &declared {
        match relative_to_workspace(raw, input.workspace_root) {
            Some(rel) => {
                if !inside.contains(&rel) {
                    inside.push(rel);
                }
            }
            None => {
                if !outside.contains(raw) {
                    outside.push(raw.clone());
                }
            }
        }
    }
    if inside.is_empty() && outside.is_empty() {
        return ApprovalEffects {
            changes: Vec::new(),
            changes_summary: "A file change; the files it names could not be read".into(),
            leaves_sandbox: unknown("The call does not name a file the engine could read"),
            can_be_undone: unknown("The engine cannot tell which files this changes"),
        };
    }

    let changes_summary = match (inside.len(), outside.len()) {
        (n, 0) => format!("{} in this project", file_count(n)),
        (0, k) => format!("{} outside this project", file_count(k)),
        (n, k) => format!(
            "{} in this project and {} outside it",
            file_count(n),
            file_count(k)
        ),
    };
    let leaves_sandbox = if outside.is_empty() {
        no("Only touches files in this project")
    } else {
        yes("Touches files outside this project folder")
    };
    let can_be_undone = if !outside.is_empty() {
        no("Rewind only restores files inside this project")
    } else if SNAPSHOT_TOOLS.contains(&input.tool_name) {
        yes("File edits are restored by rewind")
    } else if input.tool_name == "file_service" {
        file_service_undo(input.args)
    } else {
        unknown("Rewind may not restore this kind of change")
    };
    let mut changes = inside;
    changes.extend(outside);
    ApprovalEffects {
        changes,
        changes_summary,
        leaves_sandbox,
        can_be_undone,
    }
}

/// `file_service` changes are recorded as run changes with an undo body, except a deleted file
/// that is too large (or looks like a secret) to be kept.
fn file_service_undo(args: &Value) -> ApprovalFact {
    let operation = |value: &Value| {
        value
            .get("operation")
            .or_else(|| value.get("action"))
            .and_then(Value::as_str)
            .map(|op| op.trim().to_ascii_lowercase())
    };
    let mut ops: Vec<String> = Vec::new();
    if let Some(op) = operation(args) {
        if op == "batch" {
            if let Some(list) = args.get("operations").and_then(Value::as_array) {
                ops.extend(list.iter().filter_map(operation));
            }
        } else {
            ops.push(op);
        }
    }
    if ops.is_empty()
        || ops
            .iter()
            .any(|op| matches!(op.as_str(), "trash" | "delete"))
    {
        unknown("A deleted file is put back only when it is small enough to be kept")
    } else {
        yes("The change is recorded and can be put back")
    }
}

fn file_count(n: usize) -> String {
    if n == 1 {
        "1 file".to_string()
    } else {
        format!("{n} files")
    }
}

// ---------------------------------------------------------------------------
// Shell

fn shell_effects(input: &EffectInputs<'_>) -> ApprovalEffects {
    let leaves_sandbox = match input.sandbox {
        Some(SandboxFact::Disabled) => yes("The sandbox is off for this project"),
        Some(SandboxFact::Strict) => {
            no("Runs inside the sandbox: writes stay in the workspace and the network is off")
        }
        Some(SandboxFact::FilesConfined) => unknown(
            "Runs inside the sandbox: writes stay in the workspace, but the network is open to it",
        ),
        None => unknown("The engine cannot tell how this will be sandboxed"),
    };
    ApprovalEffects {
        changes: Vec::new(),
        changes_summary: "A shell command; the files it changes are not known in advance".into(),
        leaves_sandbox,
        can_be_undone: unknown("A shell command can change things rewind does not track"),
    }
}

// ---------------------------------------------------------------------------
// Network, browser and tool servers

fn outside_effects(input: &EffectInputs<'_>) -> ApprovalEffects {
    let host = host_of(input.args);
    let server = mcp_server_of(input.tool_name);
    let changes_summary = match (&host, &server, input.capability) {
        (Some(host), _, _) => format!("Reaches {host}"),
        (None, Some(server), _) => format!("Calls the {server} tool server"),
        (None, None, CapabilityClass::Browser) => "Acts in the browser".to_string(),
        (None, None, _) => "Reaches an outside service".to_string(),
    };
    // Local media tools (reading, transcribing) name no host and stay on this machine.
    if input.capability == CapabilityClass::Media && host.is_none() {
        return ApprovalEffects {
            changes: Vec::new(),
            changes_summary: "Processes a local media file".into(),
            leaves_sandbox: unknown("The engine cannot tell whether this reaches the network"),
            can_be_undone: unknown("The engine cannot tell whether this can be undone"),
        };
    }
    let leaves_sandbox = if host.is_some() && input.capability == CapabilityClass::Network {
        yes("Needs network access the sandbox does not allow")
    } else {
        yes("Talks to a service outside this machine")
    };
    ApprovalEffects {
        changes: Vec::new(),
        changes_summary,
        leaves_sandbox,
        can_be_undone: no("Something sent outside this machine cannot be taken back"),
    }
}

// ---------------------------------------------------------------------------
// Git

fn git_effects(input: &EffectInputs<'_>) -> ApprovalEffects {
    let operation = input
        .args
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or("");
    if operation == "push" {
        let remote = input
            .args
            .get("remote")
            .and_then(Value::as_str)
            .filter(|remote| !remote.is_empty());
        return ApprovalEffects {
            changes: Vec::new(),
            changes_summary: match remote {
                Some(remote) => format!("Pushes to the remote {remote}"),
                None => "Pushes to a remote".to_string(),
            },
            leaves_sandbox: yes("Sends commits to a service outside this machine"),
            can_be_undone: no("Something sent outside this machine cannot be taken back"),
        };
    }
    let mut changes: Vec<String> = Vec::new();
    if let Some(paths) = input.args.get("paths").and_then(Value::as_array) {
        for raw in paths.iter().filter_map(Value::as_str) {
            let shown =
                relative_to_workspace(raw, input.workspace_root).unwrap_or_else(|| raw.to_string());
            if !changes.contains(&shown) {
                changes.push(shown);
            }
        }
    }
    let changes_summary = if changes.is_empty() {
        "A change to this project's git history or settings".to_string()
    } else {
        format!("{} in this project's git data", file_count(changes.len()))
    };
    ApprovalEffects {
        changes,
        changes_summary,
        leaves_sandbox: if operation == "set_remote" {
            no("Only saves the remote's address; nothing is sent")
        } else {
            no("Only changes this project's git data")
        },
        can_be_undone: unknown("Rewind does not track git history"),
    }
}

// ---------------------------------------------------------------------------
// Reads

fn read_effects(input: &EffectInputs<'_>) -> ApprovalEffects {
    let outside = declared_paths(input.tool_name, input.args)
        .iter()
        .any(|raw| relative_to_workspace(raw, input.workspace_root).is_none());
    ApprovalEffects {
        changes: Vec::new(),
        changes_summary: "Only reads; nothing is changed".into(),
        leaves_sandbox: if outside {
            yes("Reads a path outside this project folder")
        } else {
            no("Reads inside this project")
        },
        can_be_undone: yes("Nothing is changed, so there is nothing to undo"),
    }
}

// ---------------------------------------------------------------------------
// Argument reading

/// The paths a call names, as written, in order and without duplicates.
fn declared_paths(tool_name: &str, args: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |raw: &str| {
        let raw = raw.trim();
        if !raw.is_empty() && raw != "/dev/null" && !out.iter().any(|seen| seen == raw) {
            out.push(raw.to_string());
        }
    };
    collect_path_keys(args, &mut push);
    if tool_name == "file_service" {
        if let Some(list) = args.get("operations").and_then(Value::as_array) {
            for operation in list {
                collect_path_keys(operation, &mut push);
            }
        }
    }
    if let Some(patch) = args.get("patch").and_then(Value::as_str) {
        for line in patch.lines() {
            let Some(marker) = line.strip_prefix("*** ").map(str::trim) else {
                continue;
            };
            if matches!(marker, "Begin Patch" | "End Patch" | "End of File") {
                continue;
            }
            let path = ["Update File:", "Add File:", "Delete File:", "Move to:"]
                .iter()
                .find_map(|prefix| marker.strip_prefix(prefix))
                .unwrap_or(marker);
            push(path);
        }
    }
    out
}

fn collect_path_keys(value: &Value, push: &mut impl FnMut(&str)) {
    for key in ["path", "file_path", "notebook_path", "destination", "to"] {
        if let Some(raw) = value.get(key).and_then(Value::as_str) {
            push(raw);
        }
    }
}

/// The host a call reaches, from a `host` argument or the host part of a `url` argument.
fn host_of(args: &Value) -> Option<String> {
    if let Some(host) = args.get("host").and_then(Value::as_str) {
        let host = host.trim();
        if !host.is_empty() {
            return Some(host.to_string());
        }
    }
    let url = args.get("url").and_then(Value::as_str)?.trim();
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    (!host.is_empty()).then(|| host.to_string())
}

/// The tool server behind an `mcp__<server>__<tool>` name.
fn mcp_server_of(tool_name: &str) -> Option<String> {
    let rest = tool_name.strip_prefix("mcp__")?;
    let server = rest.split("__").next().unwrap_or("");
    (!server.is_empty()).then(|| server.to_string())
}

/// Lexically resolve `raw` against the workspace and return the path relative to it, with `/`
/// separators, or `None` when it points outside. Symlinks are not followed.
fn relative_to_workspace(raw: &str, workspace_root: &Path) -> Option<String> {
    let root = normalize(workspace_root);
    let candidate = Path::new(raw);
    let absolute = if candidate.is_absolute() {
        normalize(candidate)
    } else {
        normalize(&workspace_root.join(candidate))
    };
    let rel = absolute.strip_prefix(&root).ok()?;
    let parts: Vec<String> = rel
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if parts.is_empty() {
        // The workspace folder itself.
        return Some(".".to_string());
    }
    Some(parts.join("/"))
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}
