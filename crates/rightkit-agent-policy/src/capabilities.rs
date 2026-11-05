use serde::{Deserialize, Serialize};

/// Capability classes (spec §13). Every tool is gated by exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapabilityClass {
    ReadFile,
    WriteFile,
    Shell,
    /// Native OCR/transcription/extraction/fetch tools. Kept separate from
    /// broad shell authority so a role can process media without arbitrary
    /// command execution (T04).
    Media,
    Network,
    Browser,
    Mcp,
    Secrets,
    GitWrite,
    ProcessControl,
    Destructive,
    ArtifactTrust,
    AddContext,
    /// Tool declared a class we don't recognize — treated as effectful (gated), never silently allowed.
    Unknown,
}

/// Map a tool's `permission_class` string (e.g. `"read_file"`) to a [`CapabilityClass`].
pub fn parse_capability(permission_class: Option<&str>) -> CapabilityClass {
    match permission_class {
        Some("read_file")
        | Some("list_files")
        | Some("search")
        | Some("git_read")
        | Some("git_diff")
        | Some("list_mcp_tools")
        | Some("list_mcp_sources")
        | Some("list_mcp_resources")
        | Some("read_mcp_resource")
        | Some("todo_write")
        // Pure discovery/clock meta tools: no workspace or network effect.
        | Some("tool_search")
        | Some("get_time")
        | Some("lsp")
        | Some("task_get")
        | Some("task_list") => CapabilityClass::ReadFile,
        Some("write_file") | Some("edit_file") | Some("multi_edit") | Some("undo_edit")
        | Some("apply_patch") => CapabilityClass::WriteFile,
        Some("shell") | Some("powershell") => CapabilityClass::Shell,
        Some("media")
        | Some("ocr")
        | Some("ocr_onnx")
        | Some("web_extract")
        | Some("media_fetch")
        | Some("ffmpeg_extract") => CapabilityClass::Media,
        Some("network") => CapabilityClass::Network,
        Some("browser") => CapabilityClass::Browser,
        Some("mcp") | Some("mcp_call") => CapabilityClass::Mcp,
        Some("secrets") => CapabilityClass::Secrets,
        Some("git_write") => CapabilityClass::GitWrite,
        Some("process_control") => CapabilityClass::ProcessControl,
        // Only `task_stop` controls a process — it terminates running work.
        // Creating, updating and reading a task is orchestration bookkeeping in
        // the task store, already bounded by worker caps, budget ceilings and
        // spawn-depth rules. Classing all four as ProcessControl meant AutoSafe
        // paused on `task_create`, so a sub-agent (which runs AutoSafe and has
        // no way to answer an approval) stalled on the conductor's own
        // bookkeeping call before doing any work.
        Some("task_stop") => CapabilityClass::ProcessControl,
        Some("task_create") | Some("task_update") | Some("task_output") => {
            CapabilityClass::AddContext
        }
        Some("destructive") => CapabilityClass::Destructive,
        Some("artifact_trust") => CapabilityClass::ArtifactTrust,
        Some("add_context") => CapabilityClass::AddContext,
        _ => CapabilityClass::Unknown,
    }
}

/// What a tool call does to the world (rule 50a, T-01). Shown by `caps --json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectClass {
    /// Reads local state only.
    Read,
    /// Changes local state: files, repository, processes, tasks.
    Write,
    /// Reaches outside the machine or an external tool server: network, browser, MCP, media fetch.
    External,
    /// Spends metered credits.
    Spend,
    /// Destroys data that cannot be restored.
    Destroy,
}

/// Who may approve a tool call that needs approval (rule 50a, T-01).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalClass {
    /// Never asks.
    None,
    /// Asks, and a headless caller may answer with `--yes`.
    Yes,
    /// Asks, and only a person in the app can answer.
    InApp,
}

impl EffectClass {
    pub fn as_str(self) -> &'static str {
        match self {
            EffectClass::Read => "read",
            EffectClass::Write => "write",
            EffectClass::External => "external",
            EffectClass::Spend => "spend",
            EffectClass::Destroy => "destroy",
        }
    }

    /// The approval class every call of this effect carries.
    pub fn approval_class(self) -> ApprovalClass {
        match self {
            EffectClass::Read => ApprovalClass::None,
            EffectClass::Write | EffectClass::External | EffectClass::Spend => ApprovalClass::Yes,
            EffectClass::Destroy => ApprovalClass::InApp,
        }
    }
}

impl ApprovalClass {
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalClass::None => "none",
            ApprovalClass::Yes => "yes",
            ApprovalClass::InApp => "in_app",
        }
    }
}

/// Tools that spend metered credits. Closed list: a new paid tool must be added here.
const SPEND_TOOLS: &[&str] = &["image_generate"];

/// Effect class of a tool from its declared capability class (or, when it declares none, the name
/// table) and its read-only flag. An undeclared, unrecognised tool is `Write`, the gated default,
/// unless it reports itself read-only.
pub fn effect_class_for_tool(
    name: &str,
    declared: Option<CapabilityClass>,
    read_only: bool,
) -> EffectClass {
    if SPEND_TOOLS.contains(&name) {
        return EffectClass::Spend;
    }
    let class = declared.unwrap_or_else(|| parse_capability(Some(name)));
    match class {
        CapabilityClass::ReadFile => EffectClass::Read,
        CapabilityClass::Media
        | CapabilityClass::Network
        | CapabilityClass::Browser
        | CapabilityClass::Mcp
        | CapabilityClass::Secrets => EffectClass::External,
        CapabilityClass::Destructive => EffectClass::Destroy,
        CapabilityClass::Unknown if read_only => EffectClass::Read,
        CapabilityClass::WriteFile
        | CapabilityClass::Shell
        | CapabilityClass::GitWrite
        | CapabilityClass::ProcessControl
        | CapabilityClass::ArtifactTrust
        | CapabilityClass::AddContext
        | CapabilityClass::Unknown => EffectClass::Write,
    }
}

/// Read-only classes never mutate the workspace and are allowed in every state except `Locked`.
pub fn is_read_only(cls: CapabilityClass) -> bool {
    cls == CapabilityClass::ReadFile
}

/// Classes that write the filesystem — the deny-glob write list applies to these.
pub fn is_file_write(cls: CapabilityClass) -> bool {
    matches!(cls, CapabilityClass::WriteFile | CapabilityClass::GitWrite)
}
